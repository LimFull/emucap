use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// Enforce the exchange deadline inside tungstenite's partial-frame IO loops.
pub(super) struct DeadlineStream {
    pub stream: TcpStream,
    pub deadline: Option<Instant>,
    pub poll_timeout: Duration,
}

impl DeadlineStream {
    pub fn remaining(&self) -> io::Result<Duration> {
        match self.deadline {
            Some(deadline) => deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .map(|remaining| remaining.min(self.poll_timeout))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "PPSSPP exchange deadline expired")
                }),
            None => Ok(self.poll_timeout),
        }
    }
}

impl std::ops::Deref for DeadlineStream {
    type Target = TcpStream;
    fn deref(&self) -> &TcpStream {
        &self.stream
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.remaining()?;
        #[cfg(windows)]
        crate::windows_socket::wait_readable(&self.stream, remaining, "PPSSPP")?;
        #[cfg(not(windows))]
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(buffer)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.flush()
    }
}
