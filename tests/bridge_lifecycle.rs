#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sha1::{Digest as _, Sha1};
use sha2::Sha256;

const NATIVE_DEBUG_TOKEN: &str = "native-bridge-lifecycle-test-token";
const FRONT_ROOT: &str = "bridge-lifecycle-front-root";
const LAUNCH_ID: &str = "launch-01arz3ndektsv4rrffq69g5fav";

struct ChildGuard(Child);

impl ChildGuard {
    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.0.try_wait().unwrap().is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn spawn_emulator() -> ChildGuard {
    ChildGuard(
        Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

fn spawn_bridge(emucap_port: u16, gdb_port: u16, emulator_pid: u32, front_key: &str) -> ChildGuard {
    ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_emucap-mame-pc98-bridge"))
            .arg(emucap_port.to_string())
            .arg(format!("127.0.0.1:{gdb_port}"))
            .env("EMUCAP_EMULATOR_PID", emulator_pid.to_string())
            .env("EMUCAP_NATIVE_DEBUG_TOKEN", NATIVE_DEBUG_TOKEN)
            .env("EMUCAP_SESSION_TOKEN", front_key)
            .env("EMUCAP_LAUNCH_ID", LAUNCH_ID)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

fn read_packet(stream: &mut TcpStream) -> String {
    let mut frame = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).unwrap();
        frame.push(byte[0]);
        if frame.len() >= 4 && frame[frame.len() - 3] == b'#' {
            break;
        }
    }
    let marker = frame.iter().position(|byte| *byte == b'#').unwrap();
    std::str::from_utf8(&frame[1..marker]).unwrap().to_string()
}

fn packet(payload: &str) -> Vec<u8> {
    let checksum = payload
        .bytes()
        .fold(0u8, |sum, byte| sum.wrapping_add(byte));
    format!("${payload}#{checksum:02x}").into_bytes()
}

fn hmac_sha1(key: &[u8], fields: &[&[u8]]) -> [u8; 20] {
    let mut key_block = [0u8; 64];
    if key.len() > key_block.len() {
        key_block[..20].copy_from_slice(&Sha1::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for index in 0..64 {
        inner_pad[index] ^= key_block[index];
        outer_pad[index] ^= key_block[index];
    }
    let mut inner = Sha1::new();
    inner.update(inner_pad);
    for field in fields {
        inner.update((field.len() as u64).to_be_bytes());
        inner.update(field);
    }
    let mut outer = Sha1::new();
    outer.update(outer_pad);
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn proof(direction: &[u8], client_nonce: &str, server_nonce: &str) -> String {
    hex::encode(hmac_sha1(
        NATIVE_DEBUG_TOKEN.as_bytes(),
        &[
            b"emucap-mame-native-auth-v1",
            direction,
            client_nonce.as_bytes(),
            server_nonce.as_bytes(),
        ],
    ))
}

fn hmac_sha256(key: &[u8], fields: &[&[u8]]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > key_block.len() {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for index in 0..64 {
        inner_pad[index] ^= key_block[index];
        outer_pad[index] ^= key_block[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    for field in fields {
        inner.update((field.len() as u64).to_be_bytes());
        inner.update(field);
    }
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn authenticate_front(mut stream: TcpStream, port: u16, front_key: &str) -> TcpStream {
    const CHALLENGE_SIZE: usize = 25;
    const PRODUCER_PROOF_SIZE: usize = 88;
    const DIRECTIONAL_PROOF_SIZE: usize = 39;
    let mut challenge = [0u8; CHALLENGE_SIZE];
    challenge[..9].copy_from_slice(&[
        b'E',
        b'C',
        b'F',
        b'A',
        1,
        1,
        1,
        (port >> 8) as u8,
        port as u8,
    ]);
    challenge[9..].copy_from_slice(&[0x42; 16]);
    stream.write_all(&challenge).unwrap();

    let mut producer = [0u8; PRODUCER_PROOF_SIZE];
    stream.read_exact(&mut producer).unwrap();
    assert_eq!(&producer[..7], b"ECFA\x01\x02\x01");
    assert_eq!(&producer[7..40], LAUNCH_ID.as_bytes());
    let nonce = &producer[40..56];
    let expected_nonce = hmac_sha256(
        front_key.as_bytes(),
        &[b"emucap-producer-front-nonce-v1", &challenge],
    );
    assert_eq!(nonce, &expected_nonce[..16]);
    let mut transcript = Vec::from(challenge);
    transcript.extend_from_slice(LAUNCH_ID.as_bytes());
    transcript.extend_from_slice(nonce);
    let producer_proof = hmac_sha256(
        front_key.as_bytes(),
        &[b"emucap-producer-front-auth-v1", b"producer", &transcript],
    );
    assert_eq!(&producer[56..], producer_proof);

    let mut listener = [0u8; DIRECTIONAL_PROOF_SIZE];
    listener[..7].copy_from_slice(b"ECFA\x01\x03\x01");
    listener[7..].copy_from_slice(&hmac_sha256(
        front_key.as_bytes(),
        &[b"emucap-producer-front-auth-v1", b"listener", &transcript],
    ));
    stream.write_all(&listener).unwrap();

    let mut commit = [0u8; DIRECTIONAL_PROOF_SIZE];
    stream.read_exact(&mut commit).unwrap();
    assert_eq!(&commit[..7], b"ECFA\x01\x04\x01");
    assert_eq!(
        &commit[7..],
        hmac_sha256(
            front_key.as_bytes(),
            &[b"emucap-producer-front-auth-v1", b"commit", &transcript],
        )
    );
    stream.write_all(b"ECFA\x01\x05\x01").unwrap();
    stream
}

fn accept_within(listener: &TcpListener, timeout: Duration) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for bridge connection"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("accept bridge connection: {error}"),
        }
    }
}

fn complete_initial_gdb_freeze(mut gdb: TcpStream) -> TcpStream {
    gdb.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let client_nonce = read_packet(&mut gdb)
        .strip_prefix("qEmucap,auth1:")
        .unwrap()
        .to_string();
    let server_nonce = "00112233445566778899aabbccddeeff";
    let challenge = format!(
        "qEmucap,auth2:{server_nonce}:{}",
        proof(b"server", &client_nonce, server_nonce)
    );
    gdb.write_all(b"+").unwrap();
    gdb.write_all(&packet(&challenge)).unwrap();
    let mut ack = [0u8; 1];
    gdb.read_exact(&mut ack).unwrap();
    assert_eq!(ack[0], b'+');
    assert_eq!(
        read_packet(&mut gdb),
        format!(
            "qEmucap,auth3:{}",
            proof(b"client", &client_nonce, server_nonce)
        )
    );
    gdb.write_all(b"+$OK#9a").unwrap();
    gdb.read_exact(&mut ack).unwrap();
    assert_eq!(ack[0], b'+');

    assert_eq!(read_packet(&mut gdb), "?");
    gdb.write_all(b"+$S05#b8").unwrap();
    gdb.read_exact(&mut ack).unwrap();
    assert_eq!(ack[0], b'+');

    let mut interrupt = [0u8; 1];
    gdb.read_exact(&mut interrupt).unwrap();
    assert_eq!(interrupt[0], 0x03);
    gdb.write_all(b"$S05#b8").unwrap();
    gdb.read_exact(&mut ack).unwrap();
    assert_eq!(ack[0], b'+');

    assert_eq!(read_packet(&mut gdb), "qEmucap,features,");
    gdb.write_all(b"+").unwrap();
    gdb.write_all(&packet("owned_frames_v1,halt_state_v1"))
        .unwrap();
    gdb.read_exact(&mut ack).unwrap();
    assert_eq!(ack[0], b'+');
    assert_eq!(read_packet(&mut gdb), "qEmucap,haltstate,");
    gdb.write_all(b"+").unwrap();
    gdb.write_all(&packet("frozen")).unwrap();
    gdb.read_exact(&mut ack).unwrap();
    assert_eq!(ack[0], b'+');
    gdb
}

fn connected_bridge() -> (ChildGuard, ChildGuard, TcpStream, TcpStream) {
    let front_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let gdb_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut emulator = spawn_emulator();
    let front_port = front_listener.local_addr().unwrap().port();
    let front_key = emucap::live::producer_auth::derive_front_key(
        FRONT_ROOT,
        emucap::live::producer_auth::Boundary::Direct,
        front_port,
        LAUNCH_ID,
    )
    .unwrap();
    let bridge = spawn_bridge(
        front_port,
        gdb_listener.local_addr().unwrap().port(),
        emulator.0.id(),
        &front_key,
    );
    let gdb = complete_initial_gdb_freeze(accept_within(&gdb_listener, Duration::from_secs(3)));
    let front = authenticate_front(
        accept_within(&front_listener, Duration::from_secs(3)),
        front_port,
        &front_key,
    );
    assert!(!emulator.wait_for_exit(Duration::from_millis(1)));
    (emulator, bridge, gdb, front)
}

#[test]
fn idle_bridge_exits_when_its_emulator_generation_ends() {
    let (mut emulator, mut bridge, _gdb, front) = connected_bridge();
    front
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();

    emulator.0.kill().unwrap();
    emulator.0.wait().unwrap();

    let mut reader = BufReader::new(front);
    let mut line = String::new();
    assert_eq!(
        reader.read_line(&mut line).unwrap(),
        0,
        "the bridge must release its front connection after the emulator exits"
    );
    assert!(
        bridge.wait_for_exit(Duration::from_secs(3)),
        "the bridge process must end with its emulator generation"
    );
}

#[test]
fn gdb_eof_makes_the_bridge_terminal_instead_of_reconnecting_the_front() {
    let (_emulator, mut bridge, gdb, mut front) = connected_bridge();
    drop(gdb);
    front
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    front
        .write_all(b"{\"v\":1,\"id\":17,\"method\":\"get_state\",\"params\":{}}\n")
        .unwrap();

    let mut reader = BufReader::new(front);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], 17);
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["kind"], "bridge_error");
    let mut eof = String::new();
    assert_eq!(
        reader.read_line(&mut eof).unwrap(),
        0,
        "a poisoned GDB client must not survive into another front session"
    );
    assert!(
        bridge.wait_for_exit(Duration::from_secs(3)),
        "the bridge must exit after a terminal backend error"
    );
}
