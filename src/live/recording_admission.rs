//! Bounded, nonblocking admission shared by the recording event and member sinks.
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;

const MAX_CANDIDATES: usize = 16;
const HANDSHAKE_BYTES: usize = 1024;
const CANDIDATE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Handshake {
    token: String,
    capture_id: String,
}

struct Candidate {
    reader: BufReader<TcpStream>,
    line: Vec<u8>,
    deadline: Instant,
}

pub(super) fn accept_authenticated(
    listener: &TcpListener,
    token: &str,
    capture: &str,
    deadline: Instant,
    stop: &AtomicBool,
    poll: Duration,
) -> Result<BufReader<TcpStream>, String> {
    let mut candidates = VecDeque::<Candidate>::new();
    loop {
        if stop.load(Ordering::Acquire) {
            return Err("recording sink stopped before authentication".into());
        }
        if Instant::now() >= deadline {
            return Err("recording sink authentication deadline exceeded".into());
        }
        // Limit accepts per iteration so connection floods cannot starve candidate reads.
        for _ in 0..8 {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
                    if candidates.len() == MAX_CANDIDATES {
                        candidates.pop_front();
                    }
                    candidates.push_back(Candidate {
                        reader: BufReader::with_capacity(HANDSHAKE_BYTES, stream),
                        line: Vec::new(),
                        deadline: deadline.min(Instant::now() + CANDIDATE_TIMEOUT),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                    ) =>
                {
                    continue
                }
                Err(e) => return Err(format!("recording sink accept failed: {e}")),
            }
        }
        for _ in 0..candidates.len() {
            let mut candidate = candidates.pop_front().unwrap();
            if Instant::now() >= candidate.deadline {
                continue;
            }
            let (consumed, complete) = match candidate.reader.fill_buf() {
                Ok([]) => continue,
                Ok(buf) => {
                    let end = buf.iter().position(|b| *b == b'\n');
                    let n = end.map_or(buf.len(), |index| index + 1);
                    if candidate.line.len() + n > HANDSHAKE_BYTES {
                        continue;
                    }
                    candidate.line.extend_from_slice(&buf[..n]);
                    (n, end.is_some())
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    candidates.push_back(candidate);
                    continue;
                }
                Err(_) => continue,
            };
            candidate.reader.consume(consumed);
            if complete {
                if let Ok(handshake) = serde_json::from_slice::<Handshake>(&candidate.line) {
                    if handshake.token == token && handshake.capture_id == capture {
                        candidate
                            .reader
                            .get_ref()
                            .set_nonblocking(false)
                            .map_err(|e| e.to_string())?;
                        candidate
                            .reader
                            .get_ref()
                            .set_read_timeout(Some(poll))
                            .map_err(|e| e.to_string())?;
                        return Ok(candidate.reader);
                    }
                }
                // An unauthenticated peer owns no recording lifecycle or failure outcome.
            } else {
                candidates.push_back(candidate);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
#[path = "recording_admission_lifetime_tests.rs"]
mod lifetime_tests;
#[cfg(test)]
#[path = "recording_admission_tests.rs"]
mod tests;
