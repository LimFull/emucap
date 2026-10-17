//! Emulator connection broker.
//!
//! Binds both ports atomically and exits if either cannot be acquired. SO_REUSEPORT is not used,
//! preserving bind-as-lock election.
use std::net::TcpListener;

use emucap::live::{broker, broker_link, runtime::RuntimeStore};

fn mint_and_publish_session_token(emu_port: u16) -> std::io::Result<String> {
    let store = RuntimeStore::discover()?;
    // A successful two-port bind is a new broker generation. Never reuse the previous file or a
    // stable environment override: while no broker owns both ports, an attacker can pre-bind one
    // port and solicit the old bearer from a client. Rotation here makes anything disclosed to a
    // losing/pre-bound listener unusable once the legitimate broker wins both ports.
    let token = broker::new_session_token();
    if !broker::session_token_is_valid(&token) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "broker session token is empty, oversized, or contains whitespace/control bytes",
        ));
    }
    store.write_broker_controller_token(emu_port, &token)?;
    store.write_compatibility_token(emu_port, &broker::producer_token_from_controller(&token))?;
    Ok(token)
}

fn configured_ports() -> (u16, u16) {
    let emu_port: u16 = std::env::var("EMUCAP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(47800);
    let sess_port: u16 = std::env::var("EMUCAP_BROKER_SESSION_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(emucap::analysis::live_regression::default_session_port(
            emu_port,
        ));
    (emu_port, sess_port)
}

fn elect_broker(
    emu_port: u16,
    sess_port: u16,
) -> std::io::Result<Option<(TcpListener, TcpListener)>> {
    let listeners = (|| {
        let emu = TcpListener::bind(("127.0.0.1", emu_port))?;
        let sess = TcpListener::bind(("127.0.0.1", sess_port))?;
        Ok::<_, std::io::Error>((emu, sess))
    })();
    match listeners {
        Ok(pair) => Ok(Some(pair)),
        Err(error) => {
            // Failed partial binds are already dropped. A concurrent winner may
            // still be publishing its fresh credential before serving requests.
            if error.kind() == std::io::ErrorKind::AddrInUse
                && broker_link::wait_for_session(
                    &format!("127.0.0.1:{sess_port}"),
                    std::time::Duration::from_millis(500),
                )
            {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

fn main() {
    let (emu_port, sess_port) = configured_ports();
    let (emu, sess) = match elect_broker(emu_port, sess_port) {
        Ok(Some(pair)) => pair,
        Ok(None) => return,
        Err(error) => {
            eprintln!("broker: could not own ports {emu_port}/{sess_port}; no authenticated incumbent ({error})");
            std::process::exit(1);
        }
    };

    // Provision only after this process wins both bind-as-lock ports. Losing broker candidates
    // must not race the elected broker by replacing its persistent capability.
    let session_token = match mint_and_publish_session_token(emu_port) {
        Ok(token) => token,
        Err(error) => {
            drop(sess);
            drop(emu);
            eprintln!("broker: could not provision private session token ({error})");
            std::process::exit(1);
        }
    };

    if std::env::var_os("EMUCAP_BROKER_STALE_MS").is_some() {
        eprintln!(
            "broker: EMUCAP_BROKER_STALE_MS is ignored; elapsed time never transfers control"
        );
    }
    eprintln!("broker: listening emu={emu_port} session={sess_port}");
    broker::serve(emu, sess, session_token);
}

#[cfg(test)]
#[path = "emucap-broker/tests.rs"]
mod tests;
