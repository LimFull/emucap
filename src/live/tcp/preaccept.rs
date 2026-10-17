//! Background handshake lifetime belongs to the TCP link.

use super::*;
use std::sync::mpsc::{self, TryRecvError};
use std::thread;

impl TcpLink {
    pub(super) fn finish_preaccept(&mut self, wait: Duration) -> Result<bool, LinkError> {
        let Some(pre) = self.preaccept.as_ref() else {
            return Ok(false);
        };
        let msg = if wait.is_zero() {
            match pre.rx.try_recv() {
                Ok(v) => Some(v),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(LinkError::NotConnected)),
            }
        } else {
            match pre.rx.recv_timeout(wait) {
                Ok(v) => Some(v),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => Some(Err(LinkError::NotConnected)),
            }
        };

        match msg {
            Some(Ok((conn, caps, token))) if token == self.expected_session_token() => {
                self.conn = Some(conn);
                self.caps = caps;
                self.runtime_candidates.clear();
                self.cancel_preaccept();
                self.consecutive_timeouts = 0;
                self.arm_preaccept()?;
                Ok(true)
            }
            Some(Ok(_)) => {
                self.cancel_preaccept();
                Ok(false)
            }
            Some(Err(e)) => {
                self.cancel_preaccept();
                Err(e)
            }
            None => match self
                .preaccept
                .as_ref()
                .and_then(|pre| pre.errors.try_recv().ok())
            {
                Some(error) => Err(error),
                None => Ok(false),
            },
        }
    }

    pub(super) fn arm_preaccept(&mut self) -> Result<(), LinkError> {
        if self.preaccept.is_some() {
            return Ok(());
        }
        let listener = self
            .listener
            .as_ref()
            .ok_or(LinkError::NotConnected)?
            .try_clone()
            .map_err(io_to_link)?;
        self.start_preaccept(listener);
        Ok(())
    }

    pub(super) fn start_preaccept(&mut self, listener: TcpListener) {
        let timeout = self.timeout;
        let token_source = Arc::clone(&self.preaccept_token);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let (tx, rx) = mpsc::sync_channel(1);
        let (error_tx, errors) = mpsc::sync_channel(1);
        let handle = thread::spawn(move || {
            if let Err(error) = listener.set_nonblocking(true) {
                let _ = tx.try_send(Err(io_to_link(error)));
                return;
            }
            let mut workers: Vec<JoinHandle<PreacceptResult>> = Vec::new();
            let mut winner = None;
            while !thread_stop.load(Ordering::Acquire) && winner.is_none() {
                let mut index = 0;
                while index < workers.len() {
                    if workers[index].is_finished() {
                        match workers.swap_remove(index).join() {
                            Ok(Ok(result)) => {
                                winner = Some(Ok(result));
                                break;
                            }
                            Ok(Err(error)) => {
                                let _ = error_tx.try_send(error);
                            }
                            Err(_) => {
                                let _ = error_tx.try_send(LinkError::NotConnected);
                            }
                        }
                    } else {
                        index += 1;
                    }
                }
                if winner.is_some() {
                    break;
                }
                // Bound both descriptors/threads and accept work. Invalid peers do not
                // end this supervisor, and cannot block established control calls.
                for _ in 0..8 {
                    match listener.accept() {
                        Ok((stream, _)) if workers.len() < 8 => {
                            let session_token = token_source
                                .read()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            let candidate_stop = Arc::clone(&thread_stop);
                            workers.push(thread::spawn(move || {
                                handshake_stream_with_stop(
                                    stream,
                                    timeout,
                                    &session_token,
                                    Some(&candidate_stop),
                                )
                                .map(|(conn, caps)| (conn, caps, session_token))
                            }));
                        }
                        Ok(_) => {} // Reject excess candidates without extending any deadline.
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::Interrupted
                                    | std::io::ErrorKind::ConnectionAborted
                                    | std::io::ErrorKind::ConnectionReset
                            ) =>
                        {
                            continue
                        }
                        Err(error) => {
                            winner = Some(Err(io_to_link(error)));
                            break;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            thread_stop.store(true, Ordering::Release);
            for worker in workers {
                let _ = worker.join();
            }
            if let Some(result) = winner {
                let _ = tx.try_send(result);
            }
        });
        self.preaccept = Some(Preaccept {
            rx,
            errors,
            handle,
            stop,
        });
    }

    pub(super) fn cancel_preaccept(&mut self) {
        let Some(preaccept) = self.preaccept.take() else {
            return;
        };
        preaccept.stop.store(true, Ordering::Release);
        let _ = preaccept.handle.join();
    }
}
