use std::io::{self, BufRead};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const SINK_POLL_MS: u64 = 50;

#[derive(Debug)]
pub(super) struct SinkOutcome {
    pub(super) events: u64,
    pub(super) bytes: u64,
    pub(super) first_frame: Option<u64>,
    pub(super) last_frame: Option<u64>,
    pub(super) truncated: bool,
    pub(super) error: Option<String>,
}

pub(super) struct SinkServer {
    pub(super) endpoint: String,
    pub(super) token: String,
    stop: Arc<AtomicBool>,
    result: mpsc::Receiver<SinkOutcome>,
    handle: Option<JoinHandle<()>>,
}

impl SinkServer {
    pub(super) fn spawn(
        writer: crate::bundle::publish::BoundedEventWriter,
        capture_id: &str,
        max_line_bytes: u64,
        max_host_ms: u64,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let endpoint = listener.local_addr()?.to_string();
        let token = format!(
            "sink-{}{}",
            ulid::Ulid::generate().to_string().to_ascii_lowercase(),
            ulid::Ulid::generate().to_string().to_ascii_lowercase()
        );
        let expected_token = token.clone();
        let expected_capture = capture_id.to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let (tx, rx) = mpsc::sync_channel(1);
        let handle = std::thread::spawn(move || {
            let outcome = run_sink(
                listener,
                writer,
                &expected_token,
                &expected_capture,
                max_line_bytes,
                max_host_ms,
                &thread_stop,
            );
            let _ = tx.send(outcome);
        });
        Ok(Self {
            endpoint,
            token,
            stop,
            result: rx,
            handle: Some(handle),
        })
    }

    pub(super) fn finish(mut self, max_host_ms: u64) -> SinkOutcome {
        let outcome = self
            .result
            .recv_timeout(Duration::from_millis(max_host_ms.saturating_add(500)))
            .unwrap_or_else(|_| {
                self.stop.store(true, Ordering::Release);
                SinkOutcome {
                    events: 0,
                    bytes: 0,
                    first_frame: None,
                    last_frame: None,
                    truncated: false,
                    error: Some("sink terminal deadline exceeded".into()),
                }
            });
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        outcome
    }

    /// An explicit adapter error is a pre-arm rejection: maintained producers never return an
    /// error response after installing the recording state. Stop an unused listener immediately
    /// instead of waiting the full admitted transaction deadline. Any observed event bytes keep
    /// the sink failure visible rather than being reclassified as a clean rejection.
    pub(super) fn cancel_unarmed(mut self) -> SinkOutcome {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let mut outcome = self.result.try_recv().unwrap_or(SinkOutcome {
            events: 0,
            bytes: 0,
            first_frame: None,
            last_frame: None,
            truncated: false,
            error: Some("unarmed sink did not return a terminal outcome".into()),
        });
        if outcome.events == 0
            && outcome.bytes == 0
            && outcome.first_frame.is_none()
            && outcome.last_frame.is_none()
            && !outcome.truncated
        {
            outcome.error = None;
        }
        outcome
    }
}

impl Drop for SinkServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_sink(
    listener: TcpListener,
    mut writer: crate::bundle::publish::BoundedEventWriter,
    expected_token: &str,
    expected_capture: &str,
    max_line_bytes: u64,
    max_host_ms: u64,
    stop: &AtomicBool,
) -> SinkOutcome {
    let deadline = Instant::now() + Duration::from_millis(max_host_ms);
    let mut events = 0_u64;
    let mut bytes = 0_u64;
    let mut first_frame = None;
    let mut last_frame = None;
    let mut truncated = false;
    let result = (|| -> Result<(), String> {
        let mut reader = super::recording_admission::accept_authenticated(
            &listener,
            expected_token,
            expected_capture,
            deadline,
            stop,
            Duration::from_millis(SINK_POLL_MS),
        )?;

        while let Some((line, complete)) =
            read_sink_line(&mut reader, max_line_bytes, deadline, stop)
                .map_err(|error| format!("sink event read failed: {error}"))?
        {
            if !complete {
                truncated = true;
                break;
            }
            let event: crate::bundle::event::EventEnvelope = serde_json::from_slice(&line)
                .map_err(|error| format!("sink event JSON failed: {error}"))?;
            writer
                .write_record(&line)
                .map_err(|error| format!("sink bounded write failed: {error}"))?;
            first_frame.get_or_insert(event.frame);
            last_frame = Some(event.frame);
            events = events.saturating_add(1);
            bytes = bytes.saturating_add(line.len() as u64);
        }
        Ok(())
    })();
    let sync_error = writer
        .finish()
        .err()
        .map(|error| format!("sink final sync failed: {error}"));
    SinkOutcome {
        events,
        bytes,
        first_frame,
        last_frame,
        truncated,
        error: match (result.err(), sync_error) {
            (Some(primary), Some(sync)) => Some(format!("{primary}; {sync}")),
            (Some(primary), None) => Some(primary),
            (None, Some(sync)) => Some(sync),
            (None, None) => None,
        },
    }
}

pub(super) fn read_sink_line<R: BufRead>(
    reader: &mut R,
    limit: u64,
    deadline: Instant,
    stop: &AtomicBool,
) -> io::Result<Option<(Vec<u8>, bool)>> {
    let mut line = Vec::with_capacity(256);
    loop {
        if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "sink deadline"));
        }
        let available = match reader.fill_buf() {
            Ok(value) => value,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some((line, false)))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        let next = (line.len() as u64)
            .checked_add(take as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sink line overflow"))?;
        if next > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sink line exceeds admitted limit",
            ));
        }
        line.extend_from_slice(&available[..take]);
        let complete = available[take - 1] == b'\n';
        reader.consume(take);
        if complete {
            return Ok(Some((line, true)));
        }
    }
}
