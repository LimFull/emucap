#[cfg(any(unix, windows))]
fn main() -> anyhow::Result<()> {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Instant;

    use anyhow::{anyhow, Context};
    use emucap::azahar_adapter::AzaharHost;
    use emucap::live::reconnect::serve_reconnecting_controlled;

    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 8 {
        return Err(anyhow!("usage: emucap-azahar <PORT> <CONTENT> <CORE> <EXPECTED_CORE_SHA256> <RUNTIME_HOME> <UPSTREAM_VERSION> <PATCHSET_SHA256>"));
    }
    let port = args[1]
        .parse::<u16>()
        .context("PORT must be a non-zero decimal port")?;
    if port == 0 {
        return Err(anyhow!("PORT must be non-zero"));
    }
    let mut host = AzaharHost::open(
        Path::new(&args[2]),
        Path::new(&args[3]),
        &args[4],
        Path::new(&args[5]),
        &args[6],
        &args[7],
    )?;
    let (command_tx, command_rx) = mpsc::channel::<emucap::azahar_adapter::dispatch::Command>();
    let terminal = Arc::new(AtomicBool::new(false));
    let server_terminal = Arc::clone(&terminal);
    let server = std::thread::spawn(move || {
        let mut control = emucap::azahar_adapter::dispatch::Control::new(command_tx);
        let probe = move || {
            server_terminal
                .load(Ordering::Acquire)
                .then(|| "Azahar owner terminated".to_string())
        };
        if let Some(runtime) = std::env::var("EMUCAP_LAUNCH_ID")
            .ok()
            .filter(|v| !v.is_empty())
        {
            emucap::live::reconnect::owned::serve_reconnecting_owned(
                port,
                "azahar-libretro",
                control,
                probe,
                emucap::live::reconnect::cancellation::TemporalAdmission {
                    runtime,
                    methods: vec!["step".into()],
                },
            )
        } else {
            serve_reconnecting_controlled(
                port,
                "azahar-libretro",
                move |request| control.legacy(request),
                probe,
            )
        }
    });

    loop {
        let command = if host.is_running() {
            match host.next_frame_start(Instant::now()) {
                // Unlimited pacing still services one queued command between frames.
                None => match command_rx.try_recv() {
                    Ok(command) => Some(command),
                    Err(mpsc::TryRecvError::Empty) => {
                        host.run_scheduled_frame()?;
                        None
                    }
                    Err(mpsc::TryRecvError::Disconnected) => break,
                },
                Some(start) => {
                    let now = Instant::now();
                    if now >= start {
                        host.run_scheduled_frame()?;
                        None
                    } else {
                        match command_rx.recv_timeout(start - now) {
                            Ok(command) => Some(command),
                            Err(mpsc::RecvTimeoutError::Timeout) => None,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                }
            }
        } else {
            match command_rx.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            }
        };
        if let Some(command) = command {
            host.process_command(command);
            if host.backend_terminal() {
                break;
            }
        }
    }
    terminal.store(true, Ordering::Release);
    server
        .join()
        .map_err(|_| anyhow!("Azahar reconnect server panicked"))??;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the Azahar frontend requires a Unix or Windows host")
}
