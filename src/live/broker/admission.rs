//! Bounded nonblocking handshakes; registry ownership begins only after authentication.
use std::collections::VecDeque;
use std::io::{self, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{Shared, MAX_ADMISSION_FRAME_BYTES, MAX_PENDING_ADMISSIONS, WRITE_TIMEOUT};
use crate::live::protocol::{read_ndjson_chunk_with_limit, to_line, Request, PROTOCOL_VERSION};

const PER_ROLE_LIMIT: usize = MAX_PENDING_ADMISSIONS / 2;
const ACCEPTS_PER_TURN: usize = 8;
const IO_CHUNK: usize = 8192;
const READS_PER_TURN: usize = MAX_ADMISSION_FRAME_BYTES / IO_CHUNK + 1;
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
pub(super) enum Role {
    Producer,
    Controller,
}

#[derive(Clone)]
enum Stage {
    ProducerProof {
        challenge: crate::live::producer_auth::ListenerChallenge,
    },
    ProducerCommit {
        auth: crate::live::producer_auth::AuthenticatedTranscript,
    },
    ProducerHello {
        launch_id: String,
    },
    ControllerInit,
    ControllerAttach {
        client: String,
        server: String,
    },
    Closing,
}

struct PendingWrite {
    bytes: Vec<u8>,
    offset: usize,
}

impl PendingWrite {
    fn line(mut line: String) -> Self {
        if !line.ends_with('\n') {
            line.push('\n');
        }
        Self {
            bytes: line.into_bytes(),
            offset: 0,
        }
    }

    fn bytes(bytes: Vec<u8>) -> Self {
        Self { bytes, offset: 0 }
    }

    fn poll(&mut self, writer: &mut impl Write) -> io::Result<bool> {
        let end = self.bytes.len().min(self.offset + IO_CHUNK);
        match writer.write(&self.bytes[self.offset..end]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => self.offset += written,
            Err(error) if retryable(&error) => return Ok(false),
            Err(error) => return Err(error),
        }
        Ok(self.offset == self.bytes.len())
    }
}

struct Candidate {
    reader: BufReader<TcpStream>,
    pending: Vec<u8>,
    output: Option<PendingWrite>,
    stage: Stage,
    deadline: Instant,
}

enum Progress {
    Pending,
    Closed,
    Authenticated(Value),
}

fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

impl Candidate {
    fn new(stream: TcpStream, role: Role) -> io::Result<Self> {
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        stream.set_nonblocking(true)?;
        let (stage, output) = match role {
            Role::Producer => {
                let port = stream.local_addr()?.port();
                let challenge = crate::live::producer_auth::new_listener_challenge(
                    crate::live::producer_auth::Boundary::BrokerProducer,
                    port,
                )?;
                let output = PendingWrite::bytes(
                    crate::live::producer_auth::challenge_bytes(&challenge).to_vec(),
                );
                (Stage::ProducerProof { challenge }, Some(output))
            }
            Role::Controller => (Stage::ControllerInit, None),
        };
        Ok(Self {
            reader: BufReader::with_capacity(IO_CHUNK, stream),
            pending: Vec::new(),
            output,
            stage,
            deadline,
        })
    }

    fn poll(&mut self, token: &str) -> io::Result<Progress> {
        if Instant::now() >= self.deadline {
            return Ok(Progress::Closed);
        }
        if let Some(output) = &mut self.output {
            if !output.poll(self.reader.get_mut())? {
                return Ok(Progress::Pending);
            }
            self.output = None;
        }
        if matches!(self.stage, Stage::Closing) {
            return Ok(Progress::Closed);
        }
        match self.stage.clone() {
            Stage::ProducerProof { challenge } => {
                let record =
                    match self.read_record(crate::live::producer_auth::producer_proof_size())? {
                        Some(record) => record,
                        None => return Ok(Progress::Pending),
                    };
                let port = self.reader.get_ref().local_addr()?.port();
                let auth = crate::live::producer_auth::verify_producer_proof(
                    token,
                    &challenge,
                    crate::live::producer_auth::Boundary::BrokerProducer,
                    port,
                    &record,
                )?;
                self.output = Some(PendingWrite::bytes(
                    crate::live::producer_auth::listener_proof_record(&auth).to_vec(),
                ));
                self.stage = Stage::ProducerCommit { auth };
                Ok(Progress::Pending)
            }
            Stage::ProducerCommit { auth } => {
                let record =
                    match self.read_record(crate::live::producer_auth::producer_commit_size())? {
                        Some(record) => record,
                        None => return Ok(Progress::Pending),
                    };
                crate::live::producer_auth::verify_producer_commit(&auth, &record)?;
                let launch_id = auth.launch_id().to_string();
                let mut output = crate::live::producer_auth::listener_ok_record(
                    crate::live::producer_auth::Boundary::BrokerProducer,
                )
                .to_vec();
                output.extend_from_slice(to_line(&Request::new(0, "hello", json!({}))).as_bytes());
                self.output = Some(PendingWrite::bytes(output));
                self.stage = Stage::ProducerHello { launch_id };
                Ok(Progress::Pending)
            }
            Stage::ProducerHello { launch_id } => {
                let value = match self.read_json_frame()? {
                    Some(value) => value,
                    None => return Ok(Progress::Pending),
                };
                Ok(producer_result(value, &launch_id)
                    .map_or(Progress::Closed, Progress::Authenticated))
            }
            Stage::ControllerInit => {
                let value = match self.read_json_frame()? {
                    Some(value) => value,
                    None => return Ok(Progress::Pending),
                };
                let Some(client) = client_nonce(&value) else {
                    return Ok(Progress::Closed);
                };
                let server = super::new_auth_nonce();
                self.output = Some(PendingWrite::line(
                    json!({
                        "id": 0, "ok": true, "result": {
                            "protocol_version": PROTOCOL_VERSION,
                            "server_nonce": server,
                            "server_proof": super::session_server_proof(token, &client, &server),
                        }
                    })
                    .to_string(),
                ));
                self.stage = Stage::ControllerAttach { client, server };
                Ok(Progress::Pending)
            }
            Stage::ControllerAttach { client, server } => {
                let value = match self.read_json_frame()? {
                    Some(value) => value,
                    None => return Ok(Progress::Pending),
                };
                match attach_request(value, token, &client, &server) {
                    Ok(request) => Ok(Progress::Authenticated(request)),
                    Err(response) => {
                        self.output = Some(PendingWrite::line(response.to_string()));
                        self.stage = Stage::Closing;
                        Ok(Progress::Pending)
                    }
                }
            }
            Stage::Closing => unreachable!(),
        }
    }

    fn read_record(&mut self, size: usize) -> io::Result<Option<Vec<u8>>> {
        if self.pending.len() >= size {
            return Ok(Some(self.pending.drain(..size).collect()));
        }
        let remaining = size - self.pending.len();
        let mut bytes = vec![0u8; remaining.min(IO_CHUNK)];
        match self.reader.read(&mut bytes) {
            Ok(0) => Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => {
                self.pending.extend_from_slice(&bytes[..read]);
                if self.pending.len() == size {
                    Ok(Some(self.pending.drain(..size).collect()))
                } else {
                    Ok(None)
                }
            }
            Err(error) if retryable(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn read_json_frame(&mut self) -> io::Result<Option<Value>> {
        let line = match self.read_frame() {
            Ok(Some(line)) => line,
            Ok(None) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Err(error) if retryable(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        serde_json::from_str(line.trim())
            .map(Some)
            .map_err(Into::into)
    }

    fn read_frame(&mut self) -> io::Result<Option<String>> {
        // A complete maximum-size frame already in the socket must fit in one turn before
        // new accepts can evict this candidate. Bound syscall work too: a slow fragmented
        // sender gets another turn rather than keeping this scheduler inside its read loop.
        for _ in 0..READS_PER_TURN {
            let before = self.pending.len();
            match read_ndjson_chunk_with_limit(
                &mut self.reader,
                &mut self.pending,
                MAX_ADMISSION_FRAME_BYTES,
            ) {
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && self.pending.len() > before => {
                }
                result => return result,
            }
        }
        Err(io::ErrorKind::WouldBlock.into())
    }

    fn into_reader(self) -> io::Result<BufReader<TcpStream>> {
        // Never discard the buffer: the peer may pipeline bytes after its authenticated frame.
        if Instant::now() >= self.deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.reader.get_ref().set_nonblocking(false)?;
        self.reader.get_ref().set_read_timeout(None)?;
        self.reader
            .get_ref()
            .set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(self.reader)
    }
}

fn producer_result(value: Value, authenticated_launch_id: &str) -> Option<Value> {
    if value.get("id").and_then(Value::as_u64) != Some(0)
        || value.get("ok").and_then(Value::as_bool) != Some(true)
    {
        return None;
    }
    let mut result = value.get("result")?.as_object()?.clone();
    if result.get("protocol_version").and_then(Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
        || result.get("session_token").is_some()
        || result
            .get("launch_id")
            .and_then(Value::as_str)
            .is_some_and(|launch_id| launch_id != authenticated_launch_id)
    {
        return None;
    }
    result.insert(
        "launch_id".into(),
        Value::String(authenticated_launch_id.to_string()),
    );
    Some(Value::Object(result))
}

fn client_nonce(value: &Value) -> Option<String> {
    if value.get("v").and_then(Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
        || value.get("id").and_then(Value::as_u64) != Some(0)
        || value.get("method").and_then(Value::as_str) != Some("broker_authenticate")
        || value
            .get("params")?
            .get("protocol_version")
            .and_then(Value::as_u64)
            != Some(u64::from(PROTOCOL_VERSION))
    {
        return None;
    }
    let nonce = value.get("params")?.get("client_nonce")?.as_str()?;
    super::auth_nonce_is_current(nonce).then(|| nonce.to_owned())
}

fn attach_request(value: Value, token: &str, client: &str, server: &str) -> Result<Value, Value> {
    let req_id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
    let proof = value
        .get("params")
        .and_then(|params| params.get("auth_proof"))
        .and_then(Value::as_str);
    let binding = value
        .get("params")
        .and_then(Value::as_object)
        .map(super::session_attach_binding);
    let authenticated = proof
        .zip(binding.as_deref())
        .is_some_and(|(proof, binding)| {
            super::auth_proof_matches(
                &super::session_client_proof(token, client, server, binding),
                proof,
            )
        });
    let error = if !authenticated {
        Some(("identity_mismatch", "broker authentication failed"))
    } else if value.get("v").and_then(Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
        || value.get("id").and_then(Value::as_u64) != Some(1)
        || value.get("method").and_then(Value::as_str) != Some("attach")
    {
        Some((
            "bad_request",
            "the authenticated broker session request must be attach",
        ))
    } else {
        None
    };
    if let Some((kind, message)) = error {
        Err(json!({"id":req_id,"ok":false,"error":{"kind":kind,"message":message}}))
    } else {
        Ok(value)
    }
}

fn service(candidates: &mut VecDeque<Candidate>, reg: &Shared, token: &str, role: Role) {
    for _ in 0..candidates.len() {
        let mut candidate = candidates.pop_front().expect("candidate count");
        match candidate.poll(token) {
            Ok(Progress::Pending) => candidates.push_back(candidate),
            Ok(Progress::Authenticated(value)) => {
                if let Ok(reader) = candidate.into_reader() {
                    let reg = Arc::clone(reg);
                    let _ = std::thread::Builder::new()
                        .name("emucap-broker-authenticated".into())
                        .spawn(move || match role {
                            Role::Producer => super::handle_emulator(reader, reg, value),
                            Role::Controller => super::handle_session(reader, reg, value),
                        });
                }
            }
            Ok(Progress::Closed) | Err(_) => {}
        }
    }
}

pub(super) fn serve(
    listener: TcpListener,
    reg: Shared,
    token: Arc<str>,
    role: Role,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let mut candidates = VecDeque::new();
    loop {
        // Service ready peers before accepting/evicting. Accept work cannot starve existing I/O.
        service(&mut candidates, &reg, &token, role);
        accept_candidates(&listener, &mut candidates, role);
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn accept_candidates(listener: &TcpListener, candidates: &mut VecDeque<Candidate>, role: Role) {
    for _ in 0..ACCEPTS_PER_TURN {
        match listener.accept() {
            Ok((stream, _)) => {
                // Only unauthenticated candidates are evicted. The other role has its own cap.
                if candidates.len() == PER_ROLE_LIMIT {
                    candidates.pop_front();
                }
                if let Ok(candidate) = Candidate::new(stream, role) {
                    candidates.push_back(candidate);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                ) =>
            {
                continue
            }
            // Resource pressure (for example EMFILE) can clear when candidates expire.
            // Keep the listener alive and yield to candidate I/O/deadlines before retrying.
            Err(_) => break,
        }
    }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
