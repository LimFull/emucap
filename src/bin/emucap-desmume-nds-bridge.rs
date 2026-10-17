use std::time::Duration;

use anyhow::{anyhow, Context};
use emucap::gdb_rsp::{GdbBridgeEnv, GdbRspClient};
use emucap::live::reconnect::{serve_reconnecting_controlled, BridgeReply, ProcessDependency};
use emucap::nds_bridge::NdsBridge;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage: emucap-desmume-nds-bridge <EMUCAP_PORT> <ARM9_HOST:PORT> [ARM7_HOST:PORT]"
        );
        eprintln!(
            "  ARM9 GDB endpoint (DeSmuME --arm9gdb) is required; ARM7 (--arm7gdb) is optional."
        );
        std::process::exit(2);
    }
    let emucap_port = parse_port(&args[1]).context("invalid EMUCAP_PORT")?;
    let (arm9_host, arm9_port) = parse_endpoint(&args[2]).context("invalid ARM9 GDB endpoint")?;
    let arm7_endpoint = match args.get(3) {
        Some(raw) => Some(parse_endpoint(raw).context("invalid ARM7 GDB endpoint")?),
        None => None,
    };
    let bridge_env = GdbBridgeEnv::from_process_env();
    bridge_env
        .session_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            anyhow!("EMUCAP_SESSION_TOKEN must be set and non-empty for the emucap control session")
        })?;
    let native_debug_token = std::env::var("EMUCAP_NATIVE_DEBUG_TOKEN")
        .context("EMUCAP_NATIVE_DEBUG_TOKEN is required for DeSmuME GDB authentication")?;
    if native_debug_token.is_empty() {
        return Err(anyhow!(
            "EMUCAP_NATIVE_DEBUG_TOKEN must be non-empty for DeSmuME GDB authentication"
        ));
    }

    eprintln!(
        "[desmume-nds-rust] connecting arm9={arm9_host}:{arm9_port} emucap=127.0.0.1:{emucap_port}"
    );
    let mut arm9 = GdbRspClient::connect(
        &arm9_host,
        arm9_port,
        Duration::from_secs(5),
        Duration::from_secs(30),
    )
    .with_context(|| format!("connect ARM9 GDB stub at {arm9_host}:{arm9_port}"))?;
    arm9.authenticate(&native_debug_token)
        .context("authenticate ARM9 GDB stub")?;

    let arm7 = match &arm7_endpoint {
        Some((host, port)) => {
            eprintln!("[desmume-nds-rust] connecting arm7={host}:{port}");
            let mut client =
                GdbRspClient::connect(host, *port, Duration::from_secs(5), Duration::from_secs(30))
                    .with_context(|| format!("connect ARM7 GDB stub at {host}:{port}"))?;
            client
                .authenticate(&native_debug_token)
                .context("authenticate ARM7 GDB stub")?;
            Some(client)
        }
        None => {
            eprintln!("[desmume-nds-rust] no ARM7 endpoint; arm7 memory/cpu routing disabled");
            None
        }
    };

    let mut bridge = NdsBridge::new(arm9, arm7, bridge_env);
    let dependency =
        ProcessDependency::from_process_env().context("load emulator process dependency")?;

    if let Some(runtime) = std::env::var("EMUCAP_LAUNCH_ID")
        .ok()
        .filter(|v| !v.is_empty())
    {
        bridge
            .enable_owned_control()
            .map_err(|e| anyhow!(e.to_string()))?;
        return emucap::live::reconnect::owned::serve_reconnecting_owned(
            emucap_port,
            "desmume-nds-rust",
            bridge,
            move || {
                dependency
                    .as_ref()
                    .and_then(ProcessDependency::terminal_reason)
            },
            emucap::live::reconnect::cancellation::TemporalAdmission {
                runtime,
                methods: vec!["step".into()],
            },
        )
        .context("serve owned NDS session");
    }

    serve_reconnecting_controlled(
        emucap_port,
        "desmume-nds-rust",
        move |request| {
            let response = bridge.handle_request(request);
            if bridge.backend_terminal() {
                BridgeReply::terminate_with(response)
            } else {
                BridgeReply::continue_with(response)
            }
        },
        move || {
            dependency
                .as_ref()
                .and_then(ProcessDependency::terminal_reason)
        },
    )
    .context("serve reconnecting emucap session")
}

fn parse_port(raw: &str) -> anyhow::Result<u16> {
    raw.parse::<u16>()
        .map_err(|_| anyhow!("expected decimal TCP port, got {raw:?}"))
        .and_then(|port| {
            if port == 0 {
                Err(anyhow!("port must be in 1..=65535"))
            } else {
                Ok(port)
            }
        })
}

fn parse_endpoint(raw: &str) -> anyhow::Result<(String, u16)> {
    let (host, port) = raw
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("expected GDB endpoint as HOST:PORT"))?;
    if host.is_empty() {
        return Err(anyhow!("GDB host is empty"));
    }
    let address = host
        .parse::<std::net::IpAddr>()
        .map_err(|_| anyhow!("GDB host must be a numeric loopback address, got {host:?}"))?;
    if !address.is_loopback() {
        return Err(anyhow!(
            "GDB host must be loopback so the native debugger credential is never sent off-host"
        ));
    }
    Ok((address.to_string(), parse_port(port)?))
}

#[cfg(test)]
#[path = "emucap-desmume-nds-bridge/tests.rs"]
mod tests;
