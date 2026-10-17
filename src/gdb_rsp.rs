//! Adapter-neutral GDB Remote Serial Protocol transport and process metadata.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sha1::{Digest as _, Sha1};

#[cfg(test)]
#[path = "gdb_rsp_tests.rs"]
pub(crate) mod tests;

#[derive(Debug, thiserror::Error)]
pub enum GdbError {
    #[error("{0}")]
    Emulator(String),
    #[error("GDB transport is poisoned after a prior stream error")]
    Poisoned,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type GdbResult<T> = Result<T, GdbError>;

/// One demultiplexing operation, including packets skipped before its terminal reply.
pub(crate) struct ExchangeBudget {
    deadline: Instant,
    exhausted: std::cell::Cell<bool>,
    packets: usize,
    bytes: usize,
}

impl ExchangeBudget {
    pub(crate) fn new(timeout: Duration) -> GdbResult<Self> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| GdbError::Emulator("GDB exchange timeout overflow".into()))?;
        Ok(Self {
            deadline,
            exhausted: std::cell::Cell::new(false),
            packets: 0,
            bytes: 0,
        })
    }

    pub(crate) fn remaining(&self) -> GdbResult<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|value| !value.is_zero())
            .ok_or_else(|| {
                self.exhausted.set(true);
                GdbError::Emulator("GDB demultiplexing deadline expired".into())
            })
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.exhausted.get()
    }

    pub(crate) fn skipped(&mut self, packet: &str) -> GdbResult<()> {
        self.remaining()?;
        if self.packets >= 1024 || packet.len() > (8 * 1024 * 1024) - self.bytes {
            self.exhausted.set(true);
            return Err(GdbError::Emulator(
                "GDB demultiplexing budget exceeded".into(),
            ));
        }
        self.packets += 1;
        self.bytes += packet.len();
        Ok(())
    }
}

pub trait GdbTransport {
    fn send(&mut self, payload: &str) -> GdbResult<String>;
    /// Bounds the entire request/response, including fragmented packets and ACKs.
    fn send_with_timeout(&mut self, _payload: &str, _budget: Duration) -> GdbResult<String> {
        Err(GdbError::Emulator("bounded send unsupported".into()))
    }
    fn recv_reply_with_timeout(&mut self, _budget: Duration) -> GdbResult<String> {
        Err(GdbError::Emulator("bounded receive unsupported".into()))
    }
    fn send_no_reply(&mut self, payload: &str) -> GdbResult<()>;
    fn send_no_reply_with_timeout(&mut self, _payload: &str, _budget: Duration) -> GdbResult<()> {
        Err(GdbError::Emulator("bounded dispatch unsupported".into()))
    }
    fn interrupt(&mut self) -> GdbResult<String>;
    fn request_interrupt_with_timeout(&mut self, _budget: Duration) -> GdbResult<()> {
        Err(GdbError::Emulator(
            "bounded interrupt request unsupported".into(),
        ))
    }
    fn interrupt_with_timeout(&mut self, _budget: Duration) -> GdbResult<String> {
        Err(GdbError::Emulator("bounded interrupt unsupported".into()))
    }
    /// True once this transport can no longer carry another request. Front-session reconnect must
    /// not reuse a terminal backend connection.
    fn is_terminal(&self) -> bool {
        false
    }
    /// Reads the next RSP packet without first writing a command.
    ///
    /// Adapter demultiplexers use this after discarding an asynchronous stop that arrived before
    /// the actual command response.
    fn recv_reply(&mut self) -> GdbResult<String> {
        Err(GdbError::Emulator("recv_reply unsupported".into()))
    }
    fn get_timeout(&self) -> GdbResult<Duration> {
        Ok(Duration::from_secs(5))
    }
    fn set_timeout(&mut self, _timeout: Duration) -> GdbResult<()> {
        Ok(())
    }
    fn recv_nonblocking_with_timeout(&mut self, _budget: Duration) -> GdbResult<Option<String>> {
        Err(GdbError::Emulator("bounded polling unsupported".into()))
    }
    fn recv_nonblocking(&mut self) -> GdbResult<Option<String>> {
        Ok(None)
    }
}

/// Process identity and content metadata shared by GDB-backed adapters.
#[derive(Debug, Clone, Default)]
pub struct GdbBridgeEnv {
    pub name: Option<String>,
    pub session_token: Option<String>,
    pub launch_id: Option<String>,
    pub content: Option<PathBuf>,
    pub build: Option<String>,
}

impl GdbBridgeEnv {
    pub fn from_process_env() -> Self {
        Self {
            name: std::env::var("EMUCAP_NAME").ok(),
            session_token: std::env::var("EMUCAP_SESSION_TOKEN").ok(),
            launch_id: std::env::var("EMUCAP_LAUNCH_ID").ok(),
            content: std::env::var_os("EMUCAP_CONTENT").map(PathBuf::from),
            build: Some(crate::build_identity::BUILD_HASH.to_string()),
        }
    }
}

// Bound encoded RSP payloads before decoding; framing adds four bytes.
const MAX_RSP_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_RSP_PREFIX_BYTES: usize = 64 * 1024;

pub struct GdbRspClient {
    stream: TcpStream,
    buf: VecDeque<u8>,
    poisoned: bool,
    io_deadline: Option<Instant>,
    timeout: Duration,
}

impl GdbRspClient {
    pub fn connect(
        host: &str,
        port: u16,
        timeout: Duration,
        connect_wait: Duration,
    ) -> std::io::Result<Self> {
        let deadline = Instant::now() + connect_wait;
        loop {
            match TcpStream::connect((host, port)) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(timeout))?;
                    stream.set_write_timeout(Some(timeout))?;
                    return Ok(Self {
                        stream,
                        buf: VecDeque::new(),
                        poisoned: false,
                        io_deadline: None,
                        timeout,
                    });
                }
                Err(err) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(300));
                    if err.kind() == std::io::ErrorKind::InvalidInput {
                        return Err(err);
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Start RSP on a stream that already completed the xemu native-authentication exchange.
    pub fn from_authenticated_stream(
        stream: TcpStream,
        timeout: Duration,
    ) -> std::io::Result<Self> {
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(Self {
            stream,
            buf: VecDeque::new(),
            poisoned: false,
            io_deadline: None,
            timeout,
        })
    }

    /// Authenticate an already-connected emucap-managed GDB stub before any
    /// ordinary RSP command is sent. Generic GDB-backed adapters continue to
    /// use `connect` without this extension; DeSmuME requires it on each CPU
    /// socket. Any failed or unexpected exchange makes the connection unusable.
    pub fn authenticate(&mut self, native_debug_token: &str) -> GdbResult<()> {
        if native_debug_token.is_empty() {
            self.poisoned = true;
            return Err(GdbError::Emulator(
                "native debugger token must be non-empty for GDB authentication".into(),
            ));
        }

        let payload = format!(
            "qEmucap,auth:{}",
            hex::encode(native_debug_token.as_bytes())
        );
        match self.send(&payload) {
            Ok(reply) if reply == "OK" => Ok(()),
            Ok(_) => {
                self.poisoned = true;
                Err(GdbError::Emulator(
                    "GDB stub rejected native debugger authentication".into(),
                ))
            }
            Err(error) => {
                self.poisoned = true;
                Err(error)
            }
        }
    }

    /// Mutually authenticate the MAME native-control hop. The bridge validates
    /// the producer before it returns a client proof, so a process that merely
    /// won the predictable loopback port cannot collect a reusable bearer.
    pub fn authenticate_mutual(&mut self, native_debug_token: &str) -> GdbResult<()> {
        if native_debug_token.is_empty()
            || native_debug_token.len() > 256
            || native_debug_token.as_bytes().contains(&0)
        {
            self.poisoned = true;
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
            return Err(GdbError::Emulator(
                "native debugger token is outside the MAME authentication bounds".into(),
            ));
        }

        let client_nonce = hex::encode(ulid::Ulid::generate().to_bytes());
        let result = (|| {
            let reply = self.send(&format!("qEmucap,auth1:{client_nonce}"))?;
            let fields = reply.strip_prefix("qEmucap,auth2:").ok_or_else(|| {
                GdbError::Emulator(
                    "MAME native server returned an invalid authentication reply".into(),
                )
            })?;
            let (server_nonce, server_proof) = fields.split_once(':').ok_or_else(|| {
                GdbError::Emulator(
                    "MAME native server returned an incomplete authentication proof".into(),
                )
            })?;
            if !is_hex(server_nonce, 32) || !is_hex(server_proof, 40) {
                return Err(GdbError::Emulator(
                    "MAME native server authentication fields are outside bounds".into(),
                ));
            }
            let expected =
                native_auth_proof(native_debug_token, b"server", &client_nonce, server_nonce);
            if !constant_time_equal(expected.as_bytes(), server_proof.as_bytes()) {
                return Err(GdbError::Emulator(
                    "MAME native server proof did not match this launch".into(),
                ));
            }
            let client_proof =
                native_auth_proof(native_debug_token, b"client", &client_nonce, server_nonce);
            if self.send(&format!("qEmucap,auth3:{client_proof}"))? != "OK" {
                return Err(GdbError::Emulator(
                    "MAME native server rejected the client proof".into(),
                ));
            }
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
        }
        result
    }

    fn checksum(payload: &[u8]) -> u8 {
        payload.iter().fold(0u8, |sum, b| sum.wrapping_add(*b))
    }

    fn frame(payload: &str) -> Vec<u8> {
        let data = payload.as_bytes();
        let mut out = Vec::with_capacity(data.len() + 4);
        out.push(b'$');
        out.extend_from_slice(data);
        out.push(b'#');
        out.extend_from_slice(format!("{:02x}", Self::checksum(data)).as_bytes());
        out
    }

    fn remaining_budget(&self) -> std::io::Result<Option<Duration>> {
        self.io_deadline
            .map(|deadline| {
                deadline
                    .checked_duration_since(Instant::now())
                    .filter(|remaining| !remaining.is_zero())
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "GDB exchange deadline expired",
                        )
                    })
            })
            .transpose()
    }

    fn write_bytes(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            if let Some(remaining) = self.remaining_budget()? {
                self.stream
                    .set_write_timeout(Some(remaining))
                    .map_err(|error| {
                        std::io::Error::new(
                            error.kind(),
                            format!("GDB write timeout {remaining:?}: {error}"),
                        )
                    })?;
            }
            match self.stream.write(bytes) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => bytes = &bytes[n..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn bounded<T>(
        &mut self,
        budget: Duration,
        operation: impl FnOnce(&mut Self) -> GdbResult<T>,
    ) -> GdbResult<T> {
        self.ensure_usable()?;
        let deadline = Instant::now()
            .checked_add(budget)
            .filter(|_| !budget.is_zero())
            .ok_or_else(|| GdbError::Emulator("invalid GDB exchange budget".into()))?;
        let read_timeout = self.stream.read_timeout()?;
        let write_timeout = self.stream.write_timeout()?;
        let previous_deadline = self.io_deadline;
        self.io_deadline = Some(previous_deadline.map_or(deadline, |outer| outer.min(deadline)));
        let result = operation(self);
        let elapsed = self.remaining_budget();
        self.io_deadline = previous_deadline;
        let restore_read = self.stream.set_read_timeout(read_timeout).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("GDB restore read timeout {read_timeout:?}: {error}"),
            )
        });
        let restore_write = self
            .stream
            .set_write_timeout(write_timeout)
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("GDB restore write timeout {write_timeout:?}: {error}"),
                )
            });
        if result.is_err() || elapsed.is_err() || restore_read.is_err() || restore_write.is_err() {
            self.poisoned = true;
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
        }
        if let Err(primary) = result {
            if restore_read.is_ok() && restore_write.is_ok() {
                return Err(primary);
            }
            let cleanup =
                format!("timeout restoration: read={restore_read:?}, write={restore_write:?}");
            return Err(match primary {
                GdbError::Io(error) => GdbError::Io(std::io::Error::new(
                    error.kind(),
                    format!("{error}; {cleanup}"),
                )),
                GdbError::Emulator(message) => GdbError::Emulator(format!("{message}; {cleanup}")),
                GdbError::Poisoned => GdbError::Poisoned,
            });
        }
        restore_read?;
        restore_write?;
        elapsed?;
        result
    }

    fn read_byte(&mut self) -> std::io::Result<u8> {
        let remaining = self.remaining_budget()?;
        if let Some(b) = self.buf.pop_front() {
            return Ok(b);
        }
        if let Some(remaining) = remaining {
            self.stream
                .set_read_timeout(Some(remaining))
                .map_err(|error| {
                    std::io::Error::new(
                        error.kind(),
                        format!("GDB read timeout {remaining:?}: {error}"),
                    )
                })?;
        }
        let mut chunk = [0u8; 4096];
        let n = self.stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "GDB connection closed",
            ));
        }
        self.buf.extend(&chunk[..n]);
        Ok(self.buf.pop_front().expect("buffer was just filled"))
    }

    fn write_packet(&mut self, payload: &str) -> std::io::Result<()> {
        if payload.len() > MAX_RSP_PAYLOAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "GDB payload exceeds 8 MiB",
            ));
        }
        let frame = Self::frame(payload);
        self.write_bytes(&frame)?;
        for _ in 0..8 {
            match self.read_byte()? {
                b'+' => return Ok(()),
                b'-' => self.write_bytes(&frame)?,
                b'$' => {
                    self.buf.push_front(b'$');
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "GDB packet was not acknowledged",
        ))
    }

    fn read_packet(&mut self) -> std::io::Result<String> {
        let mut prefix = 0;
        while self.read_byte()? != b'$' {
            prefix += 1;
            if prefix > MAX_RSP_PREFIX_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "GDB framing prefix exceeds byte limit",
                ));
            }
        }

        let mut raw = Vec::new();
        loop {
            let b = self.read_byte()?;
            if b == b'#' {
                break;
            }
            if raw.len() == MAX_RSP_PAYLOAD_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "GDB payload exceeds 8 MiB",
                ));
            }
            raw.push(b);
        }
        let mut checksum = [0u8; 2];
        checksum[0] = self.read_byte()?;
        checksum[1] = self.read_byte()?;
        let expected = std::str::from_utf8(&checksum)
            .ok()
            .and_then(|s| u8::from_str_radix(s, 16).ok());
        if expected != Some(Self::checksum(&raw)) {
            let _ = self.write_bytes(b"-");
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "GDB packet checksum mismatch",
            ));
        }
        self.write_bytes(b"+")?;

        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'}' && i + 1 < raw.len() {
                out.push(raw[i + 1] ^ 0x20);
                i += 2;
            } else {
                out.push(raw[i]);
                i += 1;
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn ensure_usable(&self) -> GdbResult<()> {
        if self.poisoned {
            Err(GdbError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn finish_io<T>(&mut self, result: std::io::Result<T>) -> GdbResult<T> {
        if result.is_err() {
            self.poisoned = true;
        }
        result.map_err(GdbError::from)
    }

    fn has_complete_buffered_packet(&self) -> bool {
        let Some(start) = self.buf.iter().position(|byte| *byte == b'$') else {
            return false;
        };
        self.buf
            .iter()
            .enumerate()
            .skip(start + 1)
            .find(|(_, byte)| **byte == b'#')
            .is_some_and(|(end, _)| self.buf.len() >= end + 3)
    }

    fn check_pending_packet_size(&self) -> std::io::Result<()> {
        let prefix = self
            .buf
            .iter()
            .position(|byte| *byte == b'$')
            .unwrap_or(self.buf.len());
        if prefix > MAX_RSP_PREFIX_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "GDB framing prefix exceeds byte limit",
            ));
        }
        if self.buf.len() > MAX_RSP_PAYLOAD_BYTES + 4 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "GDB incomplete packet exceeds 8 MiB payload budget",
            ));
        }
        Ok(())
    }

    fn recv_nonblocking_packet(&mut self) -> std::io::Result<Option<String>> {
        // A complete buffered reply remains valid even if the peer has since closed.
        if self.has_complete_buffered_packet() {
            return self.read_packet().map(Some);
        }
        self.check_pending_packet_size()?;
        let previous = self.stream.read_timeout()?;
        self.stream.set_nonblocking(true)?;
        let read = {
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "GDB connection closed",
                )),
                Ok(n) => {
                    self.buf.extend(&chunk[..n]);
                    Ok(())
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
                Err(err) => Err(err),
            }
        };
        self.stream.set_nonblocking(false)?;
        self.stream.set_read_timeout(previous).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("GDB poll restore timeout {previous:?}: {error}"),
            )
        })?;
        read?;
        // Never enter the blocking decoder on a partial header, body or checksum.
        // Retain the prefix so a later poll can finish this same packet.
        if !self.has_complete_buffered_packet() {
            self.check_pending_packet_size()?;
            return Ok(None);
        }
        self.read_packet().map(Some)
    }
}

fn is_hex(value: &str, size: usize) -> bool {
    value.len() == size && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hmac_sha1(key: &[u8], fields: &[&[u8]]) -> [u8; 20] {
    const BLOCK_BYTES: usize = 64;
    let mut key_block = [0u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        key_block[..20].copy_from_slice(&Sha1::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; BLOCK_BYTES];
    let mut outer_pad = [0x5cu8; BLOCK_BYTES];
    for index in 0..BLOCK_BYTES {
        inner_pad[index] ^= key_block[index];
        outer_pad[index] ^= key_block[index];
    }
    let mut inner = Sha1::new();
    inner.update(inner_pad);
    for field in fields {
        inner.update((field.len() as u64).to_be_bytes());
        inner.update(field);
    }
    let inner_digest = inner.finalize();
    let mut outer = Sha1::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn native_auth_proof(
    token: &str,
    direction: &[u8],
    client_nonce: &str,
    server_nonce: &str,
) -> String {
    hex::encode(hmac_sha1(
        token.as_bytes(),
        &[
            b"emucap-mame-native-auth-v1",
            direction,
            client_nonce.as_bytes(),
            server_nonce.as_bytes(),
        ],
    ))
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for (index, value) in left.iter().enumerate() {
        difference |= usize::from(*value ^ right.get(index).copied().unwrap_or(0));
    }
    difference == 0
}

impl GdbTransport for GdbRspClient {
    fn is_terminal(&self) -> bool {
        self.poisoned
    }

    fn send(&mut self, payload: &str) -> GdbResult<String> {
        if self.io_deadline.is_none() {
            return self.bounded(self.timeout, |client| client.send(payload));
        }
        self.ensure_usable()?;
        let result = (|| {
            self.write_packet(payload)?;
            self.read_packet()
        })();
        self.finish_io(result)
    }

    fn send_with_timeout(&mut self, payload: &str, budget: Duration) -> GdbResult<String> {
        self.bounded(budget, |client| client.send(payload))
    }

    fn recv_reply_with_timeout(&mut self, budget: Duration) -> GdbResult<String> {
        self.bounded(budget, |client| client.recv_reply())
    }

    fn send_no_reply_with_timeout(&mut self, payload: &str, budget: Duration) -> GdbResult<()> {
        self.bounded(budget, |client| client.send_no_reply(payload))
    }

    fn send_no_reply(&mut self, payload: &str) -> GdbResult<()> {
        if self.io_deadline.is_none() {
            return self.bounded(self.timeout, |client| client.send_no_reply(payload));
        }
        self.ensure_usable()?;
        let result = self.write_packet(payload);
        self.finish_io(result)
    }

    fn recv_reply(&mut self) -> GdbResult<String> {
        if self.io_deadline.is_none() {
            return self.bounded(self.timeout, |client| client.recv_reply());
        }
        self.ensure_usable()?;
        let result = self.read_packet();
        self.finish_io(result)
    }

    fn request_interrupt_with_timeout(&mut self, budget: Duration) -> GdbResult<()> {
        self.bounded(budget, |client| Ok(client.write_bytes(&[3])?))
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        if self.io_deadline.is_none() {
            return self.bounded(self.timeout, |client| client.interrupt());
        }
        self.ensure_usable()?;
        let result = self.write_bytes(&[0x03]);
        self.finish_io(result)?;
        // A GDB remote interrupt is not a request packet: the stub answers the raw 0x03 byte
        // asynchronously with a stop packet. Read and acknowledge that packet before writing
        // anything else. Sending `?` first makes bounded stubs interpret its leading `$` as the
        // missing ACK for the stop reply, after which they may close the connection.
        self.recv_reply()
    }

    fn interrupt_with_timeout(&mut self, budget: Duration) -> GdbResult<String> {
        self.bounded(budget, |client| client.interrupt())
    }

    fn get_timeout(&self) -> GdbResult<Duration> {
        Ok(self.timeout)
    }

    fn set_timeout(&mut self, timeout: Duration) -> GdbResult<()> {
        self.stream.set_read_timeout(Some(timeout))?;
        self.stream.set_write_timeout(Some(timeout))?;
        self.timeout = timeout;
        Ok(())
    }

    fn recv_nonblocking_with_timeout(&mut self, budget: Duration) -> GdbResult<Option<String>> {
        self.bounded(budget, |client| client.recv_nonblocking())
    }

    fn recv_nonblocking(&mut self) -> GdbResult<Option<String>> {
        if self.io_deadline.is_none() {
            return self.bounded(self.timeout, |client| client.recv_nonblocking());
        }
        self.ensure_usable()?;
        let result = self.recv_nonblocking_packet();
        self.finish_io(result)
    }
}
