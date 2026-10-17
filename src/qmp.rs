//! Bounded QEMU Machine Protocol transport shared by QEMU-derived adapters.

use std::collections::VecDeque;
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::live::protocol::read_ndjson_chunk;

const MAX_PENDING_EVENTS: usize = 1024;
const MAX_PENDING_EVENT_BYTES: usize = 8 * 1024 * 1024;

#[cfg(test)]
#[path = "qmp_tests.rs"]
mod tests;

#[derive(Debug, thiserror::Error)]
pub enum QmpError {
    #[error("QMP command failed ({class}): {description}")]
    Emulator { class: String, description: String },
    #[error("QMP protocol error: {0}")]
    Protocol(String),
    #[error("QMP transport is poisoned after a prior stream or protocol error")]
    Poisoned,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type QmpResult<T> = Result<T, QmpError>;

/// Single-flight QMP request transport. Emulator-declared command errors leave the connection
/// usable; framing, JSON, EOF, and response-identity failures poison it.
pub trait QmpTransport {
    fn execute(&mut self, command: &str, arguments: Option<Value>) -> QmpResult<Value>;

    fn drain_events(&mut self) -> Vec<Value> {
        Vec::new()
    }

    fn is_terminal(&self) -> bool {
        false
    }
}

pub struct QmpClient {
    stream: BufReader<TcpStream>,
    pending: Vec<u8>,
    events: VecDeque<Value>,
    event_bytes: usize,
    timeout: Duration,
    next_id: u64,
    poisoned: bool,
}

impl QmpClient {
    pub fn connect(
        host: &str,
        port: u16,
        timeout: Duration,
        connect_wait: Duration,
    ) -> std::io::Result<Self> {
        let deadline = Instant::now() + connect_wait;
        loop {
            let stream = match TcpStream::connect((host, port)) {
                Ok(stream) => stream,
                Err(error) if Instant::now() < deadline => {
                    if error.kind() == std::io::ErrorKind::InvalidInput {
                        return Err(error);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(error) => return Err(error),
            };
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))?;

            let mut client = Self {
                stream: BufReader::new(stream),
                pending: Vec::new(),
                events: VecDeque::new(),
                event_bytes: 0,
                timeout,
                next_id: 1,
                poisoned: false,
            };
            match client.initialize() {
                Ok(()) => return Ok(client),
                Err(QmpError::Io(error))
                    if retryable_handshake_io(&error) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("QMP handshake failed: {error}"),
                    ));
                }
            }
        }
    }

    /// Start QMP on a stream that already completed the xemu native-authentication exchange.
    /// The stream must still contain the complete QMP greeting.
    pub fn from_authenticated_stream(
        stream: TcpStream,
        timeout: Duration,
    ) -> std::io::Result<Self> {
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let mut client = Self {
            stream: BufReader::new(stream),
            pending: Vec::new(),
            events: VecDeque::new(),
            event_bytes: 0,
            timeout,
            next_id: 1,
            poisoned: false,
        };
        client.initialize().map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("QMP handshake failed after native authentication: {error}"),
            )
        })?;
        Ok(client)
    }

    fn initialize(&mut self) -> QmpResult<()> {
        let deadline = Instant::now() + self.timeout;
        let (greeting, _) = self.read_value(deadline)?;
        if greeting.get("QMP").and_then(Value::as_object).is_none() {
            return self.protocol_error("server greeting does not contain a QMP object");
        }
        self.execute_inner("qmp_capabilities", None, deadline)?;
        Ok(())
    }

    fn ensure_usable(&self) -> QmpResult<()> {
        if self.poisoned {
            Err(QmpError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn protocol_error<T>(&mut self, message: impl Into<String>) -> QmpResult<T> {
        self.poisoned = true;
        let _ = self.stream.get_ref().shutdown(std::net::Shutdown::Both);
        Err(QmpError::Protocol(message.into()))
    }

    fn stream_error<T>(&mut self, error: impl Into<QmpError>) -> QmpResult<T> {
        self.poisoned = true;
        let _ = self.stream.get_ref().shutdown(std::net::Shutdown::Both);
        Err(error.into())
    }

    fn remaining(&mut self, deadline: Instant) -> QmpResult<Duration> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return self.stream_error(std::io::Error::from(std::io::ErrorKind::TimedOut));
        }
        Ok(remaining)
    }

    fn write_value(&mut self, value: &Value, deadline: Instant) -> QmpResult<()> {
        let mut bytes = match serde_json::to_vec(value) {
            Ok(bytes) => bytes,
            Err(error) => return self.stream_error(error),
        };
        bytes.push(b'\n');
        let mut written = 0;
        while written < bytes.len() {
            let remaining = self.remaining(deadline)?;
            self.stream.get_ref().set_write_timeout(Some(remaining))?;
            match self.stream.get_mut().write(&bytes[written..]) {
                Ok(0) => {
                    return self.stream_error(std::io::Error::from(std::io::ErrorKind::WriteZero))
                }
                Ok(count) => written += count,
                Err(error) => return self.stream_error(error),
            }
        }
        Ok(())
    }

    fn read_value(&mut self, deadline: Instant) -> QmpResult<(Value, usize)> {
        let line = loop {
            let remaining = self.remaining(deadline)?;
            self.stream.get_ref().set_read_timeout(Some(remaining))?;
            match read_ndjson_chunk(&mut self.stream, &mut self.pending) {
                Ok(Some(line)) => break line,
                Ok(None) => {
                    return self.stream_error(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "QMP connection closed",
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(error) => return self.stream_error(error),
            }
        };
        self.remaining(deadline)?;
        match serde_json::from_str(&line) {
            Ok(value) => Ok((value, line.len())),
            Err(error) => self.stream_error(error),
        }
    }

    fn execute_inner(
        &mut self,
        command: &str,
        arguments: Option<Value>,
        deadline: Instant,
    ) -> QmpResult<Value> {
        self.ensure_usable()?;
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or_else(|| {
            self.poisoned = true;
            QmpError::Protocol("request id space exhausted".into())
        })?;

        let mut request = json!({"execute": command, "id": id});
        if let Some(arguments) = arguments {
            if !arguments.is_object() {
                return Err(QmpError::Protocol(
                    "QMP command arguments must be a JSON object".into(),
                ));
            }
            request
                .as_object_mut()
                .expect("request is an object")
                .insert("arguments".into(), arguments);
        }
        self.write_value(&request, deadline)?;

        loop {
            let (response, bytes) = self.read_value(deadline)?;
            if response.get("event").is_some() {
                if self.events.len() >= MAX_PENDING_EVENTS
                    || bytes > MAX_PENDING_EVENT_BYTES.saturating_sub(self.event_bytes)
                {
                    return self.protocol_error("pending event budget exceeded");
                }
                self.event_bytes += bytes;
                self.events.push_back(response);
                continue;
            }
            let response_id = response.get("id").and_then(Value::as_u64);
            if response_id != Some(id) {
                return self.protocol_error(format!(
                    "response id mismatch: expected {id}, got {}",
                    response
                        .get("id")
                        .map(Value::to_string)
                        .unwrap_or_else(|| "missing".into())
                ));
            }
            if let Some(result) = response.get("return") {
                return Ok(result.clone());
            }
            if let Some(error) = response.get("error") {
                let class = error
                    .get("class")
                    .and_then(Value::as_str)
                    .unwrap_or("GenericError")
                    .to_string();
                let description = error
                    .get("desc")
                    .and_then(Value::as_str)
                    .unwrap_or("QMP command failed")
                    .to_string();
                return Err(QmpError::Emulator { class, description });
            }
            return self.protocol_error("response contains neither return nor error");
        }
    }
}

fn retryable_handshake_io(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::BrokenPipe
    )
}

impl QmpTransport for QmpClient {
    fn execute(&mut self, command: &str, arguments: Option<Value>) -> QmpResult<Value> {
        self.execute_inner(command, arguments, Instant::now() + self.timeout)
    }

    fn drain_events(&mut self) -> Vec<Value> {
        self.event_bytes = 0;
        self.events.drain(..).collect()
    }

    fn is_terminal(&self) -> bool {
        self.poisoned
    }
}
