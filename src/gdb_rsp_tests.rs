use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

#[test]
fn process_env_cannot_override_the_embedded_adapter_build() {
    let _lock = crate::test_env::lock_env();
    let _restore = crate::test_env::EnvGuard::new(&["EMUCAP_BUILD_HASH"]);
    std::env::set_var("EMUCAP_BUILD_HASH", "forged-launch-build");

    let env = GdbBridgeEnv::from_process_env();

    assert_eq!(
        env.build.as_deref(),
        Some(crate::build_identity::BUILD_HASH)
    );
}

fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).unwrap();
        request.push(byte[0]);
        if request.len() >= 4 && request[request.len() - 3] == b'#' {
            return request;
        }
    }
}

fn request_payload(frame: &[u8]) -> &str {
    let marker = frame.iter().position(|byte| *byte == b'#').unwrap();
    std::str::from_utf8(&frame[1..marker]).unwrap()
}

#[test]
fn mame_native_auth_proof_matches_the_pinned_native_vector() {
    assert_eq!(
        super::native_auth_proof(
            "native-control-test-token",
            b"server",
            "102132435465768798a9bacbdcedfe0f",
            "00112233445566778899aabbccddeeff",
        ),
        "cd4b1bb6f36d7577ff4df790e3b62b98973be1c5"
    );
    assert_eq!(
        super::native_auth_proof(
            "native-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            b"server",
            "102132435465768798a9bacbdcedfe0f",
            "00112233445566778899aabbccddeeff",
        ),
        "bfd0896d0923c875b3677c82eaacd43d285982e6"
    );
}

#[test]
fn client_sends_acknowledged_packet_and_decodes_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert_eq!(std::str::from_utf8(&request).unwrap(), "$g#67");
        stream.write_all(b"+").unwrap();
        let payload = b"OK";
        let frame = format!(
            "$OK#{:02x}",
            payload
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
        );
        stream.write_all(frame.as_bytes()).unwrap();
        let mut ack = [0u8; 1];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');
    });

    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(client.send("g").unwrap(), "OK");
    handle.join().unwrap();
}

#[test]
fn authentication_is_the_first_packet_and_allows_followup_commands() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let auth_payload = format!("qEmucap,auth:{}", hex::encode(b"native-debug-token"));
        assert_eq!(
            read_request(&mut stream),
            GdbRspClient::frame(&auth_payload)
        );
        stream.write_all(b"+$OK#9a").unwrap();
        let mut ack = [0u8; 1];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');

        assert_eq!(read_request(&mut stream), b"$?#3f");
        stream.write_all(b"+$S05#b8").unwrap();
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');
    });

    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    client.authenticate("native-debug-token").unwrap();
    assert_eq!(client.send("?").unwrap(), "S05");
    assert!(!client.is_terminal());
    handle.join().unwrap();
}

#[test]
fn authentication_rejection_poisons_the_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_request(&mut stream);
        stream.write_all(b"+$E01#a6").unwrap();
        let mut ack = [0u8; 1];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');
    });

    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(matches!(
        client.authenticate("wrong-token"),
        Err(GdbError::Emulator(_))
    ));
    assert!(client.is_terminal());
    assert!(matches!(client.send("?"), Err(GdbError::Poisoned)));
    handle.join().unwrap();
}

#[test]
fn empty_authentication_token_fails_without_sending_a_packet() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut byte = [0u8; 1];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });

    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(matches!(
        client.authenticate(""),
        Err(GdbError::Emulator(_))
    ));
    assert!(client.is_terminal());
    drop(client);
    handle.join().unwrap();
}

#[test]
fn mutual_authentication_proves_mame_before_sending_the_client_proof() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let first = read_request(&mut stream);
        let client_nonce = request_payload(&first)
            .strip_prefix("qEmucap,auth1:")
            .unwrap();
        assert!(super::is_hex(client_nonce, 32));
        let server_nonce = "00112233445566778899aabbccddeeff";
        let server_proof =
            super::native_auth_proof("native-debug-token", b"server", client_nonce, server_nonce);
        let reply = format!("qEmucap,auth2:{server_nonce}:{server_proof}");
        stream.write_all(b"+").unwrap();
        stream.write_all(&GdbRspClient::frame(&reply)).unwrap();
        let mut ack = [0u8; 1];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');

        let third = read_request(&mut stream);
        let client_proof = request_payload(&third)
            .strip_prefix("qEmucap,auth3:")
            .unwrap();
        assert_eq!(
            client_proof,
            super::native_auth_proof("native-debug-token", b"client", client_nonce, server_nonce,)
        );
        stream.write_all(b"+$OK#9a").unwrap();
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');

        assert_eq!(read_request(&mut stream), b"$?#3f");
        stream.write_all(b"+$S05#b8").unwrap();
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');
    });

    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    client.authenticate_mutual("native-debug-token").unwrap();
    assert_eq!(client.send("?").unwrap(), "S05");
    assert!(!client.is_terminal());
    handle.join().unwrap();
}

#[test]
fn counterfeit_mame_never_receives_a_client_proof_or_ordinary_command() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let first = read_request(&mut stream);
        assert!(request_payload(&first).starts_with("qEmucap,auth1:"));
        let reply = format!(
            "qEmucap,auth2:{}:{}",
            "00112233445566778899aabbccddeeff",
            "00".repeat(20)
        );
        stream.write_all(b"+").unwrap();
        stream.write_all(&GdbRspClient::frame(&reply)).unwrap();
        let mut ack = [0u8; 1];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');
        let mut unexpected = [0u8; 1];
        assert_eq!(stream.read(&mut unexpected).unwrap(), 0);
    });

    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(matches!(
        client.authenticate_mutual("native-debug-token"),
        Err(GdbError::Emulator(_))
    ));
    assert!(client.is_terminal());
    assert!(matches!(client.send("?"), Err(GdbError::Poisoned)));
    handle.join().unwrap();
}

#[test]
fn mutual_authentication_rejects_missing_or_oversized_tokens_before_writing() {
    for token in [String::new(), "x".repeat(257)] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut byte = [0u8; 1];
            assert_eq!(stream.read(&mut byte).unwrap(), 0);
        });
        let mut client = GdbRspClient::connect(
            "127.0.0.1",
            port,
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(client.authenticate_mutual(&token).is_err());
        assert!(client.is_terminal());
        handle.join().unwrap();
    }
}

#[test]
fn interrupt_reads_and_acknowledges_async_stop_before_any_query() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut interrupt = [0u8; 1];
        stream.read_exact(&mut interrupt).unwrap();
        assert_eq!(interrupt[0], 0x03);

        stream.write_all(b"$S02#b5").unwrap();
        let mut ack = [0u8; 1];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');

        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut unexpected = [0u8; 1];
        let err = stream.read_exact(&mut unexpected).unwrap_err();
        assert!(
            matches!(
                err.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "interrupt must not send a trailing `?` request: {err}"
        );
    });

    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(client.interrupt().unwrap(), "S02");
    handle.join().unwrap();
}

#[test]
fn stream_error_poisons_only_the_failed_client_and_replacement_is_clean() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut first, _) = listener.accept().unwrap();
        assert_eq!(read_request(&mut first), b"$g#67");
        first.write_all(b"+$OK#00").unwrap();

        let (mut replacement, _) = listener.accept().unwrap();
        assert_eq!(read_request(&mut replacement), b"$g#67");
        replacement.write_all(b"+$OK#9a").unwrap();
        let mut ack = [0u8; 1];
        replacement.read_exact(&mut ack).unwrap();
        assert_eq!(ack[0], b'+');
    });

    let mut failed = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(!failed.is_terminal());
    assert!(matches!(failed.send("g"), Err(GdbError::Io(_))));
    assert!(failed.is_terminal());
    assert!(matches!(failed.send("g"), Err(GdbError::Poisoned)));
    drop(failed);

    let mut replacement = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(!replacement.is_terminal());
    assert_eq!(replacement.send("g").unwrap(), "OK");
    handle.join().unwrap();
}

#[test]
fn nonblocking_poll_retains_each_fragment_without_poisoning_the_channel() {
    for split in [1, 3, 4, 5, 6] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let packet = b"$S05#b8";
            stream.write_all(&packet[..split]).unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            stream.write_all(&packet[split..]).unwrap();
            let mut ack = [0];
            stream.read_exact(&mut ack).unwrap();
            assert_eq!(ack, [b'+']);
        });
        let mut client = GdbRspClient::connect(
            "127.0.0.1",
            port,
            Duration::from_secs(1),
            Duration::from_millis(50),
        )
        .unwrap();
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            assert_eq!(
                client
                    .recv_nonblocking_with_timeout(Duration::from_millis(100))
                    .unwrap(),
                None
            );
            if !client.buf.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!client.is_terminal());
        release_tx.send(()).unwrap();
        loop {
            if let Some(packet) = client
                .recv_nonblocking_with_timeout(Duration::from_millis(100))
                .unwrap()
            {
                assert_eq!(packet, "S05");
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!client.is_terminal());
        assert_eq!(client.get_timeout().unwrap(), Duration::from_secs(1));
        server.join().unwrap();
    }
}

#[test]
fn nonblocking_poll_decodes_buffered_packet_before_reading_later_socket_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut ack = [0];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(ack, [b'+']);
    });
    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(1),
        Duration::from_millis(50),
    )
    .unwrap();
    client.buf.extend(b"+$S05#b8");
    assert_eq!(client.recv_nonblocking().unwrap(), Some("S05".into()));
    assert!(client.buf.is_empty());
    assert!(!client.is_terminal());
    server.join().unwrap();
}

#[test]
fn bounded_exchange_retires_trickling_reply_and_restores_timeouts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        stream.write_all(b"+$").unwrap();
        // Every byte arrives within the socket timeout, but the packet never completes.
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(30));
            if stream.write_all(b"a").is_err() {
                break;
            }
        }
    });
    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(1),
    )
    .unwrap();
    let start = Instant::now();
    assert!(client
        .send_with_timeout("g", Duration::from_millis(120))
        .is_err());
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(client.is_terminal());
    assert_eq!(client.get_timeout().unwrap(), Duration::from_secs(2));
    assert_eq!(
        client.stream.write_timeout().unwrap(),
        Some(Duration::from_secs(2))
    );
    assert!(matches!(client.recv_reply(), Err(GdbError::Poisoned)));
    drop(client);
    handle.join().unwrap();
}

#[test]
fn bounded_exchange_success_allows_next_reply_and_restores_timeouts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        stream.write_all(b"+$OK#9a$S05#b8").unwrap();
        let mut acks = [0; 2];
        stream.read_exact(&mut acks).unwrap();
        assert_eq!(&acks, b"++");
    });
    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        client
            .send_with_timeout("g", Duration::from_secs(1))
            .unwrap(),
        "OK"
    );
    assert_eq!(
        client
            .recv_reply_with_timeout(Duration::from_secs(1))
            .unwrap(),
        "S05"
    );
    assert!(!client.is_terminal());
    assert_eq!(client.get_timeout().unwrap(), Duration::from_secs(2));
    handle.join().unwrap();
}

#[test]
fn bounded_dispatch_and_interrupt_do_not_consume_deferred_terminal() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(
            read_request(&mut stream),
            GdbRspClient::frame("QEmucap,framestep:2")
        );
        stream.write_all(b"+").unwrap();
        let mut raw = [0];
        stream.read_exact(&mut raw).unwrap();
        assert_eq!(raw, [3]);
        stream.write_all(b"$S02#b5").unwrap();
        stream.read_exact(&mut raw).unwrap();
        assert_eq!(raw, [b'+']);
    });
    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    client
        .send_no_reply_with_timeout("QEmucap,framestep:2", Duration::from_millis(200))
        .unwrap();
    client
        .request_interrupt_with_timeout(Duration::from_millis(200))
        .unwrap();
    assert_eq!(
        client
            .recv_reply_with_timeout(Duration::from_millis(200))
            .unwrap(),
        "S02"
    );
    assert_eq!(client.get_timeout().unwrap(), Duration::from_secs(2));
    handle.join().unwrap();
}

fn connected_pair() -> (GdbRspClient, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = GdbRspClient::connect(
        "127.0.0.1",
        listener.local_addr().unwrap().port(),
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .unwrap();
    let (peer, _) = listener.accept().unwrap();
    (client, peer)
}

#[test]
fn rsp_exact_payload_limit_preserves_following_packet() {
    let (mut client, mut peer) = connected_pair();
    let payload = "x".repeat(MAX_RSP_PAYLOAD_BYTES);
    client.buf.extend(GdbRspClient::frame(&payload));
    client.buf.extend(GdbRspClient::frame("OK"));
    assert_eq!(client.recv_reply().unwrap(), payload);
    assert_eq!(client.recv_nonblocking().unwrap(), Some("OK".into()));
    let mut ack = [0; 2];
    peer.read_exact(&mut ack).unwrap();
    assert_eq!(&ack, b"++");
    assert!(!client.is_terminal());
}

#[test]
fn oversized_rsp_retires_blocking_and_polling_connections_without_ack() {
    for (poll, complete) in [(false, true), (false, false), (true, true), (true, false)] {
        let (mut client, mut peer) = connected_pair();
        let payload = "x".repeat(MAX_RSP_PAYLOAD_BYTES + 5);
        let packet = if complete {
            GdbRspClient::frame(&payload)
        } else {
            format!("${payload}").into_bytes()
        };
        client.buf.extend(packet);
        let error = if poll {
            client.recv_nonblocking().map(|_| ()).unwrap_err()
        } else {
            client.recv_reply().map(|_| ()).unwrap_err()
        };
        assert!(matches!(error,GdbError::Io(ref e) if e.kind()==std::io::ErrorKind::InvalidData));
        assert!(client.is_terminal());
        assert!(matches!(client.send("?"), Err(GdbError::Poisoned)));
        peer.set_nonblocking(false).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap(),
            0,
            "rejected stream closes without an ACK"
        );
    }
}

#[test]
fn oversized_rsp_request_is_rejected_before_socket_write() {
    let (mut client, mut peer) = connected_pair();
    assert!(client
        .send_no_reply(&"x".repeat(MAX_RSP_PAYLOAD_BYTES + 1))
        .is_err());
    assert!(client.is_terminal());
    peer.set_nonblocking(false).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    assert_eq!(
        peer.read(&mut [0]).unwrap(),
        0,
        "rejected stream closes without a request"
    );
}

#[test]
fn bounded_failure_preserves_primary_error_when_timeout_cleanup_also_fails() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut client = GdbRspClient::connect(
        "127.0.0.1",
        port,
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .unwrap();
    let (_peer, _) = listener.accept().unwrap();
    let result: GdbResult<()> = client.bounded(Duration::from_millis(100), |client| {
        // Some kernels reject timeout restoration on a shut-down socket. Preserve
        // the primary category and message on either kernel behavior.
        client.stream.shutdown(std::net::Shutdown::Both).unwrap();
        Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "primary protocol failure").into())
    });
    let Err(GdbError::Io(error)) = result else {
        panic!("wrong primary error: {result:?}")
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("primary protocol failure"));
    assert!(client.poisoned);
}

#[test]
fn ordinary_exchange_deadline_cannot_be_refreshed_by_fragment_drip() {
    let (mut client, mut peer) = connected_pair();
    client.set_timeout(Duration::from_millis(100)).unwrap();
    let worker = std::thread::spawn(move || {
        read_request(&mut peer);
        peer.write_all(b"+$").unwrap();
        for _ in 0..50 {
            if peer.write_all(b"x").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let start = Instant::now();
    assert!(client.send("g").is_err());
    assert!(start.elapsed() < Duration::from_millis(400));
    assert!(client.is_terminal());
    worker.join().unwrap();
}

#[test]
fn framing_prefix_is_bounded_for_blocking_and_polling_reads() {
    for polling in [false, true] {
        let (mut client, mut peer) = connected_pair();
        client.buf.extend(vec![b'+'; MAX_RSP_PREFIX_BYTES + 1]);
        let result = if polling {
            client.recv_nonblocking().map(|_| ())
        } else {
            client.recv_reply().map(|_| ())
        };
        assert!(
            matches!(result, Err(GdbError::Io(ref error)) if error.kind() == std::io::ErrorKind::InvalidData)
        );
        assert!(client.is_terminal());
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }
}
/// Immediate adversarial packets, or a delayed stream that honors the supplied budget.
pub(crate) struct DemuxFlood {
    pub packet: &'static str,
    pub polling: bool,
    pub delayed: bool,
    pub budgets: Vec<Duration>,
    pub resumes: usize,
}

impl DemuxFlood {
    pub fn new(packet: &'static str, polling: bool, delayed: bool) -> Self {
        Self {
            packet,
            polling,
            delayed,
            budgets: Vec::new(),
            resumes: 0,
        }
    }
    fn reply(&mut self, budget: Duration) -> GdbResult<String> {
        if self.delayed {
            if let Some(previous) = self.budgets.last() {
                assert!(budget < *previous);
            }
            std::thread::sleep(budget.min(Duration::from_millis(8)));
        }
        self.budgets.push(budget);
        Ok(self.packet.into())
    }
}
impl GdbTransport for DemuxFlood {
    fn send(&mut self, _: &str) -> GdbResult<String> {
        Ok("S05".into())
    }
    fn send_no_reply(&mut self, _: &str) -> GdbResult<()> {
        self.resumes += 1;
        Ok(())
    }
    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S02".into())
    }
    fn get_timeout(&self) -> GdbResult<Duration> {
        Ok(if self.delayed {
            Duration::from_millis(30)
        } else {
            Duration::from_secs(5)
        })
    }
    fn send_with_timeout(&mut self, _: &str, budget: Duration) -> GdbResult<String> {
        self.reply(budget)
    }
    fn recv_reply_with_timeout(&mut self, budget: Duration) -> GdbResult<String> {
        self.reply(budget)
    }
    fn recv_nonblocking_with_timeout(&mut self, budget: Duration) -> GdbResult<Option<String>> {
        if self.polling {
            self.reply(budget).map(Some)
        } else {
            Ok(None)
        }
    }
}

#[test]
fn demultiplexing_budget_bounds_bytes_before_count() {
    let mut budget = ExchangeBudget::new(Duration::from_secs(1)).unwrap();
    budget.skipped(&"x".repeat(8 * 1024 * 1024)).unwrap();
    assert!(budget.skipped("x").is_err());
}
