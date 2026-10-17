//! Launch-bound authentication for xemu's raw QMP and GDB socket streams.

use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

const MAGIC: &[u8; 4] = b"ECXA";
const VERSION: u8 = 1;
const CLIENT_HELLO: u8 = 1;
const SERVER_PROOF: u8 = 2;
const CLIENT_PROOF: u8 = 3;
const SERVER_OK: u8 = 4;
const NONCE_SIZE: usize = 16;
const PROOF_SIZE: usize = 32;
const TOKEN_MAX: usize = 256;
const CLIENT_HELLO_SIZE: usize = 7 + NONCE_SIZE;
const SERVER_PROOF_SIZE: usize = 7 + NONCE_SIZE + PROOF_SIZE;
const CLIENT_PROOF_SIZE: usize = 7 + PROOF_SIZE;
const SERVER_OK_SIZE: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XemuNativeEndpoint {
    Qmp,
    Gdb,
}

impl XemuNativeEndpoint {
    fn wire_id(self) -> u8 {
        match self {
            Self::Qmp => 1,
            Self::Gdb => 2,
        }
    }

    fn domain(self) -> &'static [u8] {
        match self {
            Self::Qmp => b"emucap-xemu-qmp-auth-v1",
            Self::Gdb => b"emucap-xemu-gdb-auth-v1",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Qmp => "QMP",
            Self::Gdb => "GDB",
        }
    }
}

fn validate_token(token: &str) -> io::Result<()> {
    if token.is_empty() || token.len() > TOKEN_MAX || token.as_bytes().contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "EMUCAP_NATIVE_DEBUG_TOKEN must contain 1..=256 non-NUL bytes",
        ));
    }
    Ok(())
}

fn hash_field(hash: &mut Sha256, field: &[u8]) {
    hash.update((field.len() as u64).to_be_bytes());
    hash.update(field);
}

fn proof(
    token: &[u8],
    endpoint: XemuNativeEndpoint,
    direction: &[u8],
    client_nonce: &[u8; NONCE_SIZE],
    server_nonce: &[u8; NONCE_SIZE],
) -> [u8; PROOF_SIZE] {
    let mut key = [0u8; 64];
    if token.len() > key.len() {
        key[..PROOF_SIZE].copy_from_slice(&Sha256::digest(token));
    } else {
        key[..token.len()].copy_from_slice(token);
    }
    let mut inner_pad = [0u8; 64];
    let mut outer_pad = [0u8; 64];
    for index in 0..key.len() {
        inner_pad[index] = key[index] ^ 0x36;
        outer_pad[index] = key[index] ^ 0x5c;
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    hash_field(&mut inner, endpoint.domain());
    hash_field(&mut inner, direction);
    hash_field(&mut inner, client_nonce);
    hash_field(&mut inner, server_nonce);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for (index, value) in left.iter().enumerate() {
        difference |= usize::from(*value ^ right.get(index).copied().unwrap_or(0));
    }
    difference == 0
}

fn header(record_type: u8, endpoint: XemuNativeEndpoint) -> [u8; 7] {
    [
        MAGIC[0],
        MAGIC[1],
        MAGIC[2],
        MAGIC[3],
        VERSION,
        record_type,
        endpoint.wire_id(),
    ]
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "xemu native authentication deadline expired",
        ))
    } else {
        Ok(remaining)
    }
}

fn write_all_until(stream: &mut TcpStream, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    let mut written = 0;
    while written < bytes.len() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(&bytes[written..]) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_exact_until(stream: &mut TcpStream, bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    let mut read = 0;
    while read < bytes.len() {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        match stream.read(&mut bytes[read..]) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(count) => read += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn protocol_error(endpoint: XemuNativeEndpoint, detail: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("xemu {} authentication {detail}", endpoint.label()),
    )
}

/// Authenticate one connected raw xemu stream. The function reads exact fixed-size records, so
/// bytes following `ServerOK` remain available to the QMP or GDB client.
pub fn authenticate_stream(
    mut stream: TcpStream,
    token: &str,
    endpoint: XemuNativeEndpoint,
    timeout: Duration,
) -> io::Result<TcpStream> {
    validate_token(token)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "timeout is too large"))?;
    stream.set_nodelay(true)?;

    let result = (|| {
        let mut client_nonce = [0u8; NONCE_SIZE];
        getrandom::fill(&mut client_nonce)
            .map_err(|error| io::Error::other(format!("generate xemu client nonce: {error}")))?;
        let mut hello = [0u8; CLIENT_HELLO_SIZE];
        hello[..7].copy_from_slice(&header(CLIENT_HELLO, endpoint));
        hello[7..].copy_from_slice(&client_nonce);
        write_all_until(&mut stream, &hello, deadline)?;

        let mut challenge = [0u8; SERVER_PROOF_SIZE];
        read_exact_until(&mut stream, &mut challenge, deadline)?;
        if challenge[..7] != header(SERVER_PROOF, endpoint) {
            return Err(protocol_error(
                endpoint,
                "returned an invalid server-proof header",
            ));
        }
        let server_nonce: [u8; NONCE_SIZE] = challenge[7..7 + NONCE_SIZE]
            .try_into()
            .expect("fixed server nonce slice");
        let expected = proof(
            token.as_bytes(),
            endpoint,
            b"server",
            &client_nonce,
            &server_nonce,
        );
        if !constant_time_equal(&expected, &challenge[7 + NONCE_SIZE..]) {
            return Err(protocol_error(
                endpoint,
                "server proof did not match this launch",
            ));
        }

        let mut response = [0u8; CLIENT_PROOF_SIZE];
        response[..7].copy_from_slice(&header(CLIENT_PROOF, endpoint));
        response[7..].copy_from_slice(&proof(
            token.as_bytes(),
            endpoint,
            b"client",
            &client_nonce,
            &server_nonce,
        ));
        write_all_until(&mut stream, &response, deadline)?;

        let mut acknowledgement = [0u8; SERVER_OK_SIZE];
        read_exact_until(&mut stream, &mut acknowledgement, deadline)?;
        if acknowledgement != header(SERVER_OK, endpoint) {
            return Err(protocol_error(
                endpoint,
                "returned an invalid success record",
            ));
        }
        remaining(deadline)?;
        Ok(())
    })();

    if let Err(error) = result {
        let _ = stream.shutdown(Shutdown::Both);
        return Err(error);
    }
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

/// Connect and authenticate one native endpoint. Credential validation happens before the first
/// connection attempt. Connection refusal is retried only within `connect_wait`; protocol and proof
/// failures are terminal for this launch attempt.
pub fn connect_authenticated(
    host: &str,
    port: u16,
    token: &str,
    endpoint: XemuNativeEndpoint,
    timeout: Duration,
    connect_wait: Duration,
) -> io::Result<TcpStream> {
    validate_token(token)?;
    let deadline = Instant::now()
        .checked_add(connect_wait)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "connect wait is too large"))?;
    loop {
        match TcpStream::connect((host, port)) {
            Ok(stream) => return authenticate_stream(stream, token, endpoint, timeout),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => return Err(error),
            Err(error)
                if error.kind() == io::ErrorKind::ConnectionRefused
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
#[path = "xemu_native_auth_tests.rs"]
mod tests;
