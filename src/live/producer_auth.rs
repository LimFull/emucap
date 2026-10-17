//! Launch-bound mutual authentication for producer-to-Control transports.

use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAGIC: &[u8; 4] = b"ECFA";
const VERSION: u8 = 1;
const LISTENER_CHALLENGE: u8 = 1;
const PRODUCER_PROOF: u8 = 2;
const LISTENER_PROOF: u8 = 3;
const PRODUCER_COMMIT: u8 = 4;
const LISTENER_OK: u8 = 5;
const NONCE_SIZE: usize = 16;
const PROOF_SIZE: usize = 32;
const LAUNCH_ID_SIZE: usize = 33;
const ROOT_KEY_MAX: usize = 256;
const FRONT_KEY_HEX_SIZE: usize = PROOF_SIZE * 2;
const CHALLENGE_SIZE: usize = 4 + 1 + 1 + 1 + 2 + NONCE_SIZE;
const PRODUCER_PROOF_SIZE: usize = 4 + 1 + 1 + 1 + LAUNCH_ID_SIZE + NONCE_SIZE + PROOF_SIZE;
const DIRECTIONAL_PROOF_SIZE: usize = 4 + 1 + 1 + 1 + PROOF_SIZE;
const LISTENER_OK_SIZE: usize = 4 + 1 + 1 + 1;
const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    Direct,
    BrokerProducer,
}

impl Boundary {
    fn wire_id(self) -> u8 {
        match self {
            Self::Direct => 1,
            Self::BrokerProducer => 2,
        }
    }

    fn domain(self) -> &'static [u8] {
        match self {
            Self::Direct => b"direct",
            Self::BrokerProducer => b"broker-producer",
        }
    }

    fn key_prefix(self) -> &'static str {
        match self {
            Self::Direct => "front-direct-",
            Self::BrokerProducer => "front-broker-",
        }
    }

    pub fn from_front_key(key: &str) -> Option<Self> {
        [Self::Direct, Self::BrokerProducer]
            .into_iter()
            .find(|boundary| key.starts_with(boundary.key_prefix()))
    }
}

#[derive(Clone)]
pub(crate) struct ListenerChallenge {
    bytes: [u8; CHALLENGE_SIZE],
}

#[derive(Clone)]
pub(crate) struct AuthenticatedTranscript {
    boundary: Boundary,
    launch_id: String,
    front_key: String,
    transcript: Vec<u8>,
}

impl AuthenticatedTranscript {
    pub(crate) fn launch_id(&self) -> &str {
        &self.launch_id
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn validate_root_key(key: &str) -> io::Result<()> {
    if key.is_empty() || key.len() > ROOT_KEY_MAX || key.as_bytes().contains(&0) {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "producer authentication root must contain 1..=256 non-NUL bytes",
        ))
    } else {
        Ok(())
    }
}

fn valid_launch_id(launch_id: &str) -> bool {
    let Some(ulid) = launch_id.strip_prefix("launch-") else {
        return false;
    };
    launch_id.len() == LAUNCH_ID_SIZE
        && ulid.len() == 26
        && ulid.bytes().all(|byte| {
            matches!(
                byte,
                b'0'..=b'9'
                    | b'a'..=b'h'
                    | b'j'
                    | b'k'
                    | b'm'
                    | b'n'
                    | b'p'..=b't'
                    | b'v'..=b'z'
            )
        })
}

fn hmac_sha256(key: &[u8], fields: &[&[u8]]) -> [u8; PROOF_SIZE] {
    const BLOCK_SIZE: usize = 64;
    let mut key_block = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        key_block[..PROOF_SIZE].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; BLOCK_SIZE];
    let mut outer_pad = [0x5cu8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner_pad[index] ^= key_block[index];
        outer_pad[index] ^= key_block[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    for field in fields {
        inner.update((field.len() as u64).to_be_bytes());
        inner.update(field);
    }
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn proof_matches(expected: &[u8], presented: &[u8]) -> bool {
    let mut difference = expected.len() ^ presented.len();
    for (index, byte) in expected.iter().enumerate() {
        difference |= usize::from(*byte ^ presented.get(index).copied().unwrap_or(0));
    }
    difference == 0
}

pub fn derive_front_key(
    root_key: &str,
    boundary: Boundary,
    port: u16,
    launch_id: &str,
) -> io::Result<String> {
    validate_root_key(root_key)?;
    if !valid_launch_id(launch_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "producer authentication requires a managed launch identity",
        ));
    }
    let port = port.to_be_bytes();
    let digest = hmac_sha256(
        root_key.as_bytes(),
        &[
            b"emucap-producer-front-key-v1",
            boundary.domain(),
            &port,
            launch_id.as_bytes(),
        ],
    );
    Ok(format!("{}{}", boundary.key_prefix(), hex::encode(digest)))
}

fn validate_front_key(key: &str, boundary: Boundary) -> io::Result<()> {
    let Some(hex) = key.strip_prefix(boundary.key_prefix()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "producer authentication key has the wrong boundary",
        ));
    };
    if hex.len() != FRONT_KEY_HEX_SIZE || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "producer authentication key has an invalid format",
        ));
    }
    Ok(())
}

fn header(record_type: u8, boundary: Boundary) -> [u8; 7] {
    [
        MAGIC[0],
        MAGIC[1],
        MAGIC[2],
        MAGIC[3],
        VERSION,
        record_type,
        boundary.wire_id(),
    ]
}

fn header_matches(bytes: &[u8], record_type: u8, boundary: Boundary) -> bool {
    bytes.get(..7) == Some(header(record_type, boundary).as_slice())
}

pub(crate) fn new_listener_challenge(
    boundary: Boundary,
    port: u16,
) -> io::Result<ListenerChallenge> {
    let mut bytes = [0u8; CHALLENGE_SIZE];
    bytes[..7].copy_from_slice(&header(LISTENER_CHALLENGE, boundary));
    bytes[7..9].copy_from_slice(&port.to_be_bytes());
    getrandom::fill(&mut bytes[9..]).map_err(|error| {
        io::Error::other(format!(
            "producer authentication random source failed: {error}"
        ))
    })?;
    Ok(ListenerChallenge { bytes })
}

pub(crate) fn challenge_bytes(challenge: &ListenerChallenge) -> &[u8] {
    &challenge.bytes
}

fn producer_nonce(front_key: &str, challenge: &[u8]) -> [u8; NONCE_SIZE] {
    let digest = hmac_sha256(
        front_key.as_bytes(),
        &[b"emucap-producer-front-nonce-v1", challenge],
    );
    let mut nonce = [0u8; NONCE_SIZE];
    nonce.copy_from_slice(&digest[..NONCE_SIZE]);
    nonce
}

fn transcript(challenge: &[u8], launch_id: &str, nonce: &[u8; NONCE_SIZE]) -> Vec<u8> {
    let mut transcript = Vec::with_capacity(challenge.len() + launch_id.len() + nonce.len());
    transcript.extend_from_slice(challenge);
    transcript.extend_from_slice(launch_id.as_bytes());
    transcript.extend_from_slice(nonce);
    transcript
}

fn directional_proof(front_key: &str, direction: &[u8], transcript: &[u8]) -> [u8; PROOF_SIZE] {
    hmac_sha256(
        front_key.as_bytes(),
        &[b"emucap-producer-front-auth-v1", direction, transcript],
    )
}

fn producer_proof_record(
    challenge: &ListenerChallenge,
    front_key: &str,
    boundary: Boundary,
    launch_id: &str,
) -> ([u8; PRODUCER_PROOF_SIZE], Vec<u8>) {
    let nonce = producer_nonce(front_key, &challenge.bytes);
    let transcript = transcript(&challenge.bytes, launch_id, &nonce);
    let proof = directional_proof(front_key, b"producer", &transcript);
    let mut record = [0u8; PRODUCER_PROOF_SIZE];
    record[..7].copy_from_slice(&header(PRODUCER_PROOF, boundary));
    record[7..7 + LAUNCH_ID_SIZE].copy_from_slice(launch_id.as_bytes());
    record[7 + LAUNCH_ID_SIZE..7 + LAUNCH_ID_SIZE + NONCE_SIZE].copy_from_slice(&nonce);
    record[7 + LAUNCH_ID_SIZE + NONCE_SIZE..].copy_from_slice(&proof);
    (record, transcript)
}

pub(crate) fn verify_producer_proof(
    root_key: &str,
    challenge: &ListenerChallenge,
    boundary: Boundary,
    port: u16,
    record: &[u8],
) -> io::Result<AuthenticatedTranscript> {
    if record.len() != PRODUCER_PROOF_SIZE || !header_matches(record, PRODUCER_PROOF, boundary) {
        return Err(invalid("producer authentication proof record is invalid"));
    }
    let launch_id = std::str::from_utf8(&record[7..7 + LAUNCH_ID_SIZE])
        .map_err(|_| invalid("producer authentication launch identity is invalid"))?;
    let front_key = derive_front_key(root_key, boundary, port, launch_id)?;
    let mut nonce = [0u8; NONCE_SIZE];
    nonce.copy_from_slice(&record[7 + LAUNCH_ID_SIZE..7 + LAUNCH_ID_SIZE + NONCE_SIZE]);
    let expected_nonce = producer_nonce(&front_key, &challenge.bytes);
    if !proof_matches(&expected_nonce, &nonce) {
        return Err(invalid("producer authentication nonce is invalid"));
    }
    let transcript = transcript(&challenge.bytes, launch_id, &nonce);
    let expected = directional_proof(&front_key, b"producer", &transcript);
    if !proof_matches(&expected, &record[7 + LAUNCH_ID_SIZE + NONCE_SIZE..]) {
        return Err(invalid("producer authentication proof is invalid"));
    }
    Ok(AuthenticatedTranscript {
        boundary,
        launch_id: launch_id.to_string(),
        front_key,
        transcript,
    })
}

fn proof_record(
    record_type: u8,
    auth: &AuthenticatedTranscript,
    direction: &[u8],
) -> [u8; DIRECTIONAL_PROOF_SIZE] {
    let mut record = [0u8; DIRECTIONAL_PROOF_SIZE];
    record[..7].copy_from_slice(&header(record_type, auth.boundary));
    record[7..].copy_from_slice(&directional_proof(
        &auth.front_key,
        direction,
        &auth.transcript,
    ));
    record
}

pub(crate) fn listener_proof_record(
    auth: &AuthenticatedTranscript,
) -> [u8; DIRECTIONAL_PROOF_SIZE] {
    proof_record(LISTENER_PROOF, auth, b"listener")
}

pub(crate) fn verify_producer_commit(
    auth: &AuthenticatedTranscript,
    record: &[u8],
) -> io::Result<()> {
    if record.len() != DIRECTIONAL_PROOF_SIZE
        || !header_matches(record, PRODUCER_COMMIT, auth.boundary)
        || !proof_matches(
            &directional_proof(&auth.front_key, b"commit", &auth.transcript),
            &record[7..],
        )
    {
        return Err(invalid("producer authentication commit is invalid"));
    }
    Ok(())
}

pub(crate) fn listener_ok_record(boundary: Boundary) -> [u8; LISTENER_OK_SIZE] {
    header(LISTENER_OK, boundary)
}

pub(crate) fn producer_proof_size() -> usize {
    PRODUCER_PROOF_SIZE
}

pub(crate) fn producer_commit_size() -> usize {
    DIRECTIONAL_PROOF_SIZE
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "producer authentication deadline expired",
        ))
    } else {
        Ok(remaining)
    }
}

fn io_budget(deadline: Instant, stop: Option<&AtomicBool>) -> io::Result<Duration> {
    let remaining = remaining(deadline)?;
    Ok(if stop.is_some() {
        remaining.min(POLL_INTERVAL)
    } else {
        remaining
    }
    .max(Duration::from_millis(1)))
}

fn stopped(stop: Option<&AtomicBool>) -> bool {
    stop.is_some_and(|flag| flag.load(Ordering::Acquire))
}

fn write_all_until(
    stream: &mut TcpStream,
    bytes: &[u8],
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> io::Result<()> {
    let mut written = 0;
    while written < bytes.len() {
        if stopped(stop) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        stream.set_write_timeout(Some(io_budget(deadline, stop)?))?;
        match stream.write(&bytes[written..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => written += count,
            Err(error)
                if stop.is_some()
                    && matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_exact_until(
    stream: &mut TcpStream,
    bytes: &mut [u8],
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> io::Result<()> {
    let mut read = 0;
    while read < bytes.len() {
        if stopped(stop) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        stream.set_read_timeout(Some(io_budget(deadline, stop)?))?;
        match stream.read(&mut bytes[read..]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => read += count,
            Err(error)
                if stop.is_some()
                    && matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(crate) fn authenticate_listener_stream(
    mut stream: TcpStream,
    root_key: &str,
    boundary: Boundary,
    port: u16,
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> io::Result<(TcpStream, String)> {
    validate_root_key(root_key)?;
    stream.set_nodelay(true)?;
    let result = (|| {
        let challenge = new_listener_challenge(boundary, port)?;
        write_all_until(&mut stream, &challenge.bytes, deadline, stop)?;
        let mut producer = [0u8; PRODUCER_PROOF_SIZE];
        read_exact_until(&mut stream, &mut producer, deadline, stop)?;
        let auth = verify_producer_proof(root_key, &challenge, boundary, port, &producer)?;
        write_all_until(&mut stream, &listener_proof_record(&auth), deadline, stop)?;
        let mut commit = [0u8; DIRECTIONAL_PROOF_SIZE];
        read_exact_until(&mut stream, &mut commit, deadline, stop)?;
        verify_producer_commit(&auth, &commit)?;
        write_all_until(&mut stream, &listener_ok_record(boundary), deadline, stop)?;
        Ok(auth.launch_id)
    })();
    match result {
        Ok(launch_id) => Ok((stream, launch_id)),
        Err(error) => {
            let _ = stream.shutdown(Shutdown::Both);
            Err(error)
        }
    }
}

pub fn authenticate_producer_stream(
    mut stream: TcpStream,
    front_key: &str,
    boundary: Boundary,
    port: u16,
    launch_id: &str,
    timeout: Duration,
) -> io::Result<TcpStream> {
    validate_front_key(front_key, boundary)?;
    if !valid_launch_id(launch_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "producer authentication requires a managed launch identity",
        ));
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "timeout is too large"))?;
    stream.set_nodelay(true)?;
    let result = (|| {
        let mut challenge_bytes = [0u8; CHALLENGE_SIZE];
        read_exact_until(&mut stream, &mut challenge_bytes, deadline, None)?;
        if !header_matches(&challenge_bytes, LISTENER_CHALLENGE, boundary)
            || challenge_bytes[7..9] != port.to_be_bytes()
        {
            return Err(invalid("producer authentication challenge is invalid"));
        }
        let challenge = ListenerChallenge {
            bytes: challenge_bytes,
        };
        let (producer, transcript) =
            producer_proof_record(&challenge, front_key, boundary, launch_id);
        write_all_until(&mut stream, &producer, deadline, None)?;

        let mut listener = [0u8; DIRECTIONAL_PROOF_SIZE];
        read_exact_until(&mut stream, &mut listener, deadline, None)?;
        if !header_matches(&listener, LISTENER_PROOF, boundary)
            || !proof_matches(
                &directional_proof(front_key, b"listener", &transcript),
                &listener[7..],
            )
        {
            return Err(invalid("producer authentication listener proof is invalid"));
        }
        let auth = AuthenticatedTranscript {
            boundary,
            launch_id: launch_id.to_string(),
            front_key: front_key.to_string(),
            transcript,
        };
        write_all_until(
            &mut stream,
            &proof_record(PRODUCER_COMMIT, &auth, b"commit"),
            deadline,
            None,
        )?;
        let mut ok = [0u8; LISTENER_OK_SIZE];
        read_exact_until(&mut stream, &mut ok, deadline, None)?;
        if ok != listener_ok_record(boundary) {
            return Err(invalid("producer authentication completion is invalid"));
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            stream.set_read_timeout(None)?;
            stream.set_write_timeout(None)?;
            Ok(stream)
        }
        Err(error) => {
            let _ = stream.shutdown(Shutdown::Both);
            Err(error)
        }
    }
}

#[cfg(test)]
#[path = "producer_auth_tests.rs"]
mod tests;
