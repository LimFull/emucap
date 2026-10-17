use super::*;
use sha2::{Digest, Sha256};
use std::time::Instant;

const AUTH_CHALLENGE_COMMAND: u8 = 0xA0;
const AUTH_PROOF_COMMAND: u8 = 0xA1;
const AUTH_NONCE_SIZE: usize = 16;
const AUTH_PROOF_SIZE: usize = 32;
const AUTH_TOKEN_MAX: usize = 256;
const AUTH_DOMAIN: &[u8] = b"emucap-pcsx2-pine-auth-v1";

fn auth_hash_field(hash: &mut Sha256, field: &[u8]) {
    hash.update((field.len() as u64).to_be_bytes());
    hash.update(field);
}

fn auth_hmac(
    token: &[u8],
    direction: &[u8],
    client_nonce: &[u8; AUTH_NONCE_SIZE],
    server_nonce: &[u8; AUTH_NONCE_SIZE],
) -> [u8; AUTH_PROOF_SIZE] {
    let mut key = [0u8; 64];
    if token.len() > key.len() {
        key[..AUTH_PROOF_SIZE].copy_from_slice(&Sha256::digest(token));
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
    auth_hash_field(&mut inner, AUTH_DOMAIN);
    auth_hash_field(&mut inner, direction);
    auth_hash_field(&mut inner, client_nonce);
    auth_hash_field(&mut inner, server_nonce);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

pub trait PineTransport {
    fn transact(&mut self, request: &[u8]) -> BridgeResult<Vec<u8>>;

    fn transact_with_timeout(
        &mut self,
        request: &[u8],
        _timeout: Duration,
    ) -> BridgeResult<Vec<u8>> {
        self.transact(request)
    }

    /// True once the current PINE stream can no longer preserve frame boundaries. A caller must
    /// replace the bridge/backend generation rather than retrying on the same stream.
    fn is_terminal(&self) -> bool {
        false
    }
}

enum PineStream {
    #[cfg(windows)]
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Read for PineStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::Tcp(stream) => stream.read(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

impl Write for PineStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::Tcp(stream) => stream.write(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(windows)]
            Self::Tcp(stream) => stream.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

pub struct PineSocket {
    stream: PineStream,
    terminal: bool,
    io_deadline: Option<Instant>,
}

impl PineSocket {
    pub fn connect(slot: u16, socket_path: Option<&Path>, timeout: Duration) -> BridgeResult<Self> {
        #[cfg(windows)]
        let stream = {
            let _ = socket_path;
            let stream = TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], slot)),
                timeout,
            )?;
            // Receipt exchanges have a short, fixed deadline. Do not let small
            // control packets wait for TCP's acknowledgement batching.
            stream.set_nodelay(true)?;
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))?;
            PineStream::Tcp(stream)
        };

        #[cfg(unix)]
        let stream = {
            let _ = slot;
            let path = socket_path.ok_or_else(|| {
                Pcsx2BridgeError::BadParams(
                    "PINE socket path is required on this platform".to_string(),
                )
            })?;
            let stream = UnixStream::connect(path)?;
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))?;
            PineStream::Unix(stream)
        };

        Ok(Self {
            stream,
            terminal: false,
            io_deadline: None,
        })
    }

    pub fn authenticate_mutual(&mut self, token: &str) -> BridgeResult<()> {
        if token.is_empty() || token.len() > AUTH_TOKEN_MAX {
            return Err(Pcsx2BridgeError::BadParams(
                "EMUCAP_NATIVE_DEBUG_TOKEN must contain 1..=256 bytes".into(),
            ));
        }
        if self.terminal {
            return Err(Pcsx2BridgeError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "PINE transport is terminal",
            )));
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        let previous = self.stream.timeouts()?;
        self.io_deadline = Some(deadline);
        let outcome = (|| {
            let client_nonce = ulid::Ulid::generate().to_bytes();
            let mut challenge = Vec::with_capacity(1 + AUTH_NONCE_SIZE);
            challenge.push(AUTH_CHALLENGE_COMMAND);
            challenge.extend_from_slice(&client_nonce);
            let reply = self.transact(&challenge)?;
            if reply.len() != AUTH_NONCE_SIZE + AUTH_PROOF_SIZE {
                return Err(Pcsx2BridgeError::Protocol(format!(
                    "PINE authentication challenge returned {} bytes",
                    reply.len()
                )));
            }
            let server_nonce: [u8; AUTH_NONCE_SIZE] = reply[..AUTH_NONCE_SIZE]
                .try_into()
                .expect("checked nonce length");
            let expected = auth_hmac(token.as_bytes(), b"server", &client_nonce, &server_nonce);
            if !constant_time_equal(&expected, &reply[AUTH_NONCE_SIZE..]) {
                return Err(Pcsx2BridgeError::Protocol(
                    "PINE producer authentication failed".into(),
                ));
            }

            let proof = auth_hmac(token.as_bytes(), b"client", &client_nonce, &server_nonce);
            let mut response = Vec::with_capacity(1 + AUTH_PROOF_SIZE);
            response.push(AUTH_PROOF_COMMAND);
            response.extend_from_slice(&proof);
            let acknowledgement = self.transact(&response)?;
            if !acknowledgement.is_empty() {
                return Err(Pcsx2BridgeError::Protocol(
                    "PINE authentication acknowledgement was not empty".into(),
                ));
            }
            Ok(())
        })();
        let within_deadline = self.remaining_budget();
        self.io_deadline = None;
        let restored = self.stream.set_timeouts(previous.0, previous.1);
        if outcome.is_err() || within_deadline.is_err() || restored.is_err() {
            self.terminal = true;
        }
        restored?;
        within_deadline?;
        outcome
    }
}

impl PineTransport for PineSocket {
    fn transact(&mut self, request: &[u8]) -> BridgeResult<Vec<u8>> {
        if self.terminal {
            return Err(Pcsx2BridgeError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "PINE transport is terminal",
            )));
        }
        let outcome = (|| {
            let packet_len = request
                .len()
                .checked_add(4)
                .and_then(|length| u32::try_from(length).ok())
                .ok_or_else(|| Pcsx2BridgeError::BadParams("PINE request is too large".into()))?;
            let mut packet = Vec::with_capacity(packet_len as usize);
            packet.extend_from_slice(&packet_len.to_le_bytes());
            packet.extend_from_slice(request);
            self.write_packet_bytes(&packet)?;
            self.stream.flush()?;

            let mut header = [0u8; 5];
            self.read_packet_bytes(&mut header)?;
            let reply_len =
                u32::from_le_bytes(header[..4].try_into().expect("four bytes")) as usize;
            if !(5..=PINE_MAX_REPLY).contains(&reply_len) {
                return Err(Pcsx2BridgeError::Protocol(format!(
                    "PINE reply length {reply_len} is outside 5..={PINE_MAX_REPLY}"
                )));
            }
            let mut payload = vec![0u8; reply_len - 5];
            self.read_packet_bytes(&mut payload)?;
            if header[4] != 0 {
                return Err(Pcsx2BridgeError::Emulator(
                    "PCSX2 rejected the PINE command".into(),
                ));
            }
            Ok(payload)
        })();
        if outcome.as_ref().is_err_and(|error| {
            matches!(
                error,
                Pcsx2BridgeError::Io(_) | Pcsx2BridgeError::Protocol(_)
            )
        }) {
            self.terminal = true;
        }
        outcome
    }

    fn transact_with_timeout(
        &mut self,
        request: &[u8],
        timeout: Duration,
    ) -> BridgeResult<Vec<u8>> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or_else(|| Pcsx2BridgeError::BadParams("invalid PINE exchange budget".into()))?;
        let previous = self.stream.timeouts()?;
        self.io_deadline = Some(deadline);
        let outcome = self.transact(request);
        let within_deadline = self.remaining_budget();
        self.io_deadline = None;
        let restored = self.stream.set_timeouts(previous.0, previous.1);
        if within_deadline.is_err() || restored.is_err() {
            self.terminal = true;
        }
        restored?;
        within_deadline?;
        outcome
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }
}

impl PineStream {
    fn timeouts(&self) -> std::io::Result<(Option<Duration>, Option<Duration>)> {
        match self {
            #[cfg(windows)]
            Self::Tcp(s) => Ok((s.read_timeout()?, s.write_timeout()?)),
            #[cfg(unix)]
            Self::Unix(s) => Ok((s.read_timeout()?, s.write_timeout()?)),
        }
    }
    fn set_timeouts(&self, read: Option<Duration>, write: Option<Duration>) -> std::io::Result<()> {
        // Attempt both restorations, even if one side fails.
        let (r, w) = match self {
            #[cfg(windows)]
            Self::Tcp(s) => (s.set_read_timeout(read), s.set_write_timeout(write)),
            #[cfg(unix)]
            Self::Unix(s) => (s.set_read_timeout(read), s.set_write_timeout(write)),
        };
        r?;
        w
    }
    #[cfg(windows)]
    fn wait_readable(&self, timeout: Duration) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => {
                crate::windows_socket::wait_readable(stream, timeout, "PCSX2 PINE")
            }
        }
    }
}
impl PineSocket {
    fn remaining_budget(&self) -> std::io::Result<Option<Duration>> {
        self.io_deadline
            .map(|end| {
                end.checked_duration_since(Instant::now())
                    .filter(|left| !left.is_zero())
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "PINE exchange deadline expired",
                        )
                    })
            })
            .transpose()
    }
    fn prepare_io(&self) -> std::io::Result<()> {
        if let Some(left) = self.remaining_budget()? {
            self.stream.set_timeouts(Some(left), Some(left))?;
        }
        Ok(())
    }
    fn read_packet_bytes(&mut self, mut bytes: &mut [u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            #[cfg(windows)]
            {
                let timeout = self
                    .remaining_budget()?
                    .or(self.stream.timeouts()?.0)
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "PINE read has no finite timeout",
                        )
                    })?;
                self.stream.wait_readable(timeout)?;
            }
            self.prepare_io()?;
            match self.stream.read(bytes) {
                Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                Ok(count) => bytes = &mut bytes[count..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    fn write_packet_bytes(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            self.prepare_io()?;
            match self.stream.write(bytes) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
