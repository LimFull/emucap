//! PPSSPP (PSP) launch orchestration. Headless PPSSPP exposes CPU/memory/state through its own
//! `debugger.ppsspp.org` WebSocket (`--debugger=<port>`), and a bridge process
//! (`emucap-ppsspp-bridge`) relays that WebSocket to emucap on the MCP's port. This spawns two
//! processes: headless PPSSPP and the bridge, mirroring `adapters/ppsspp/launch.sh` (same shape as
//! DeSmuME NDS: an emulator + a bridge).

use super::{
    find_on_path, is_runnable_file, process_alive, spawn_detached, terminate_detached, LaunchSpec,
};
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Resolve the headless PPSSPPHeadless binary: `EMUCAP_PPSSPP_BIN` override, else the repo-local
/// build-headless output from `adapters/ppsspp/build.sh`. An explicit override that is not a
/// runnable file resolves to `None` (like `launch.sh`, which uses the override with no fallback).
pub fn resolve_binary(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_PPSSPP_BIN") {
        let p = PathBuf::from(explicit);
        return is_runnable_file(&p).then_some(p);
    }
    let build = repo_root.join("adapters/ppsspp/work/ppsspp/build-headless");
    let candidates = if cfg!(windows) {
        vec![
            build.join("Release/PPSSPPHeadless.exe"),
            build.join("PPSSPPHeadless.exe"),
        ]
    } else {
        vec![build.join("PPSSPPHeadless")]
    };
    candidates.into_iter().find(|path| is_runnable_file(path))
}

/// Resolve the GUI/SDL `PPSSPPSDL` binary — the HITL `display:true` build that opens a real window a
/// human can see and play while the agent drives the same debugger WebSocket. `EMUCAP_PPSSPP_GUI_BIN`
/// override, else the repo-local `PPSSPPSDL` target output from `adapters/ppsspp/build.sh` (built
/// alongside `PPSSPPHeadless` in the same `build-headless` tree). On macOS the target is an `.app`
/// bundle, so the executable lives under `Contents/MacOS`; elsewhere it is a bare `PPSSPPSDL`. An
/// explicit override that is not a runnable file resolves to `None` (like `resolve_binary`).
pub fn resolve_gui_binary(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_PPSSPP_GUI_BIN") {
        let p = PathBuf::from(explicit);
        return is_runnable_file(&p).then_some(p);
    }
    let base = repo_root.join("adapters/ppsspp/work/ppsspp/build-headless");
    [
        base.join("PPSSPPSDL.app/Contents/MacOS/PPSSPPSDL"),
        base.join("PPSSPPSDL"),
    ]
    .into_iter()
    .find(|p| is_runnable_file(p))
}

fn bridge_binary_name() -> &'static str {
    if cfg!(windows) {
        "emucap-ppsspp-bridge.exe"
    } else {
        "emucap-ppsspp-bridge"
    }
}

/// Resolve the Rust PSP bridge binary: `EMUCAP_PSP_BRIDGE_BIN` override, else the sibling of this
/// executable, `target/release`, `target/debug`, then `PATH` (matching `launch.sh`).
pub fn resolve_bridge(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_PSP_BRIDGE_BIN") {
        let p = PathBuf::from(explicit);
        return is_runnable_file(&p).then_some(p);
    }
    let name = bridge_binary_name();
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
        }
    }
    candidates.push(repo_root.join("target/release").join(name));
    candidates.push(repo_root.join("target/debug").join(name));
    if let Some(on_path) = find_on_path(name) {
        candidates.push(on_path);
    }
    candidates.into_iter().find(|p| is_runnable_file(p))
}

pub struct Launch<'a> {
    /// The headless PPSSPPHeadless binary.
    pub binary: &'a Path,
    /// The `emucap-ppsspp-bridge` binary that relays the debugger WebSocket to emucap.
    pub bridge: &'a Path,
    /// The `.iso`/`.cso`/`.pbp` content, passed as a **positional** boot argument (see `emu_spec`).
    pub content: &'a str,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<super::RuntimeEnv<'a>>,
    /// Open a native PPSSPP window (HITL viewing/play) by launching the `PPSSPPSDL` GUI build instead
    /// of `PPSSPPHeadless`; false = headless (debugger WebSocket only, the default). `binary` must
    /// already point at the GUI build when this is true (the caller resolves it via
    /// `resolve_gui_binary`). On macOS a caffeinate keeps the display awake while the window lives.
    pub display: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub ppsspp_pid: u32,
    pub bridge_pid: u32,
    pub ws_port: u16,
}

/// The PPSSPP debugger WebSocket port. `PSP_DEBUGGER_PORT` overrides it (decimal TCP port), else an
/// OS-assigned free port whose reservation listener is held until just before PPSSPP is spawned (so
/// concurrent PSP sessions never collide over a fixed/derived port, same rationale as the NDS GDB
/// ports).
#[derive(Debug)]
struct WsPort {
    port: u16,
    _reservation: Option<TcpListener>,
}

fn resolve_ws_port() -> io::Result<WsPort> {
    if let Some(raw) = std::env::var_os("PSP_DEBUGGER_PORT") {
        let value = raw.to_string_lossy();
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            let port: u16 = trimmed.parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("PSP_DEBUGGER_PORT must be a decimal TCP port, got {trimmed:?}"),
                )
            })?;
            if port == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "PSP_DEBUGGER_PORT must be in 1..=65535",
                ));
            }
            return Ok(WsPort {
                port,
                _reservation: None,
            });
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    Ok(WsPort {
        port,
        _reservation: Some(listener),
    })
}

/// Confirm a just-spawned process survives a brief startup window — an early exit (PPSSPPHeadless
/// crashing on a bad content path/flag combo, or the bridge failing on bad args / an immediate
/// connect failure) means the launch failed even though `spawn_detached` returned a pid.
fn wait_survives(pid: u32, settle: Duration, on_exit: &str) -> io::Result<()> {
    let deadline = Instant::now() + settle;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return Err(io::Error::other(on_exit.to_string()));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_owns_listening_port(pid: u32, port: u16) -> io::Result<bool> {
    let mut listening_sockets = std::collections::HashSet::new();
    let contents = std::fs::read_to_string("/proc/net/tcp")?;
    for line in contents.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() <= 9 || fields[3] != "0A" {
            continue;
        }
        let Some((encoded_address, encoded_port)) = fields[1].rsplit_once(':') else {
            continue;
        };
        if encoded_address == "0100007F" && u16::from_str_radix(encoded_port, 16).ok() == Some(port)
        {
            listening_sockets.insert(format!("socket:[{}]", fields[9]));
        }
    }
    if listening_sockets.is_empty() {
        return Ok(false);
    }

    for entry in std::fs::read_dir(format!("/proc/{pid}/fd"))? {
        let Ok(target) = std::fs::read_link(entry?.path()) else {
            continue;
        };
        if listening_sockets.contains(target.to_string_lossy().as_ref()) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
fn process_owns_listening_port(pid: u32, port: u16) -> io::Result<bool> {
    let pid = pid.to_string();
    let endpoint = format!("-iTCP@127.0.0.1:{port}");
    let output = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &pid, &endpoint, "-sTCP:LISTEN"])
        .output()?;
    Ok(output.status.success() && !output.stdout.is_empty())
}

#[cfg(windows)]
fn process_owns_listening_port(pid: u32, port: u16) -> io::Result<bool> {
    let system_root = std::env::var_os("SystemRoot").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "SystemRoot is unavailable for debugger listener ownership verification",
        )
    })?;
    let output = std::process::Command::new(
        PathBuf::from(system_root)
            .join("System32")
            .join("netstat.exe"),
    )
    .args(["-ano", "-p", "tcp"])
    .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            "netstat failed while verifying debugger listener ownership",
        ));
    }
    let expected_pid = pid.to_string();
    Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        fields.len() >= 5
            && fields[0].eq_ignore_ascii_case("TCP")
            && fields[3].eq_ignore_ascii_case("LISTENING")
            && fields[4] == expected_pid
            && fields[1]
                .rsplit_once(':')
                .is_some_and(|(host, value)| host == "127.0.0.1" && value.parse() == Ok(port))
    }))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn process_owns_listening_port(_pid: u32, _port: u16) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "debugger listener ownership verification is unsupported on this platform",
    ))
}

/// Wait until the spawned PPSSPP process itself owns the debugger WebSocket listener. Unlike the
/// NDS bridge (whose `GdbRspClient` retries the GDB stubs for 30s on its own),
/// `emucap-ppsspp-bridge`'s
/// `TungsteniteWs::connect` makes exactly one connection attempt and fails immediately — so if the
/// launcher spawned the bridge before PPSSPP opened the port, the bridge would die on a bare
/// liveness-only settle. Ownership, rather than a TCP connect probe, is security-critical: an
/// unrelated loopback listener must never receive the bridge's bearer token. Linux verifies socket
/// inodes through procfs without `lsof`; macOS and Windows use their platform listener tables.
fn wait_ws_ready(pid: u32, port: u16, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_alive(pid) {
            return Err(io::Error::other(
                "PPSSPPHeadless exited before opening the debugger WebSocket port (crashed — check the launch log)",
            ));
        }
        match process_owns_listening_port(pid, port) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => {
                return Err(io::Error::other(format!(
                    "could not verify that PPSSPPHeadless owns debugger port {port}: {error}"
                )));
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "PPSSPPHeadless did not own debugger WebSocket port {port} within {timeout:?} (refusing to release the bearer token to an unrelated listener; check the launch log)"
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Headless PPSSPP with the debugger WebSocket open. Mirrors `launch.sh`:
/// `PPSSPPHeadless --debugger=<port> --graphics=software <content>`.
///
/// Two upstream command-line constraints define this launch shape:
/// - The content is a **positional** boot argument, not `-m`/`--mount` — `-m` only mounts a
///   *second* image on `umd1:` for ELF+CSO test harnesses; passed alone it leaves PPSSPP's boot
///   list empty and nothing boots.
/// - `--timeout=<sec>` (a headless test-harness flag) aborts the run after that many wall-clock
///   seconds regardless of debugger/WebSocket activity — it is never passed here. Omitting it lets
///   PPSSPPHeadless run with no wall-clock deadline while the debugger remains interactive.
///
/// Generation-owned profile environment for the PPSSPP process, so a session — headless *or* a
/// HITL `display:true` window the human plays with PPSSPP's own control mappings — reads and writes
/// only isolated per-session state, never the operator's real PPSSPP config/saves. Applied to both
/// the headless and GUI specs (uniform; harmless where a var is unused). Three redirects, because
/// The maintained GUI and headless producers honor EMUCAP_PPSSPP_MEMSTICK before
/// creating system directories. HOME and the XDG roots also isolate auxiliary
/// configuration. Each managed generation supplies its own memory-stick root.
fn isolation_env(home: &Path) -> Vec<(String, String)> {
    let memstick = home.join("memstick");
    // `mut` is used only on non-macOS, where the XDG vars are pushed below.
    #[cfg_attr(target_os = "macos", allow(unused_mut))]
    let mut env = vec![
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        (
            "EMUCAP_PPSSPP_MEMSTICK".to_string(),
            memstick.to_string_lossy().into_owned(),
        ),
    ];
    #[cfg(not(target_os = "macos"))]
    {
        env.push((
            "XDG_CONFIG_HOME".to_string(),
            home.join("config").to_string_lossy().into_owned(),
        ));
        env.push((
            "XDG_DATA_HOME".to_string(),
            home.join("data").to_string_lossy().into_owned(),
        ));
    }
    env
}

fn prepare_profile(l: &Launch<'_>) -> io::Result<PathBuf> {
    let home = super::emu_generation_dir("ppsspp", l.port, l.runtime)?;
    crate::private_fs::ensure_private_dir_tree(&home, &home.join("memstick"))?;
    Ok(home)
}

/// Display (HITL) mode launches the `PPSSPPSDL` GUI build with the same `--debugger=<port>` and
/// positional content, but omits `--graphics=software` so the window renders with the real GPU
/// backend (Vulkan/GL). Upstream command-line handling honors `--debugger=<port>`:
/// it pins `iRemoteISOPort` to that port and arms
/// `bRemoteDebuggerOnStartup`, so the debugger WebServer binds the requested loopback port at boot.
/// Unlike headless, the GUI does not set `startBreak`, so the game boots running and the human can
/// play immediately while the agent attaches. Both modes run under an isolated generation profile
/// (`isolation_env`) so a session never reads or pollutes the operator's real PPSSPP config/saves.
fn emu_spec(
    l: &Launch,
    profile_home: &Path,
    ws_port: u16,
    native_debug_token: &str,
) -> io::Result<LaunchSpec> {
    let mut spec = LaunchSpec::new(l.binary, l.log_path)
        .arg(format!("--debugger={ws_port}"))
        .env("EMUCAP_NATIVE_DEBUG_TOKEN", native_debug_token);
    for (k, v) in isolation_env(profile_home) {
        spec = spec.env(k, v);
    }
    Ok(if l.display {
        spec.arg(l.content)
    } else {
        spec.arg("--graphics=software").arg(l.content)
    })
}

fn validate_session_token(l: &Launch) -> io::Result<()> {
    if l.session_token.is_some_and(|token| !token.is_empty()) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "EMUCAP_SESSION_TOKEN is required and must be non-empty for PPSSPP",
        ))
    }
}

/// The bridge that relays the debugger WebSocket to emucap on `l.port`. Mirrors `launch.sh`:
/// `emucap-ppsspp-bridge <port> <ws_port>` with `EMUCAP_*` env (content/name/session token —
/// `PpssppBridge::new` reads `EMUCAP_CONTENT` itself for `get_rom_info`'s sha1/size).
fn bridge_spec(l: &Launch, ws_port: u16, native_debug_token: &str) -> LaunchSpec {
    let mut spec = LaunchSpec::new(l.bridge, l.log_path)
        .arg(l.port.to_string())
        .arg(ws_port.to_string())
        .env("EMUCAP_CONTENT", l.content)
        .env("EMUCAP_NATIVE_DEBUG_TOKEN", native_debug_token);
    if let Some(name) = l.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = l.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    spec.runtime_env(l.runtime)
}

/// Spawn headless PPSSPP (debugger WebSocket), wait until it survives startup and the WebSocket
/// port is actually accepting connections, then spawn the bridge that relays it to emucap on
/// `port`. If PPSSPP never opens the port the launcher fails explicitly and cleans PPSSPP up
/// (rather than an opaque bridge connect failure). If the bridge spawn/settle fails, the launcher
/// terminates PPSSPP.
pub fn launch(l: &Launch) -> io::Result<Launched> {
    validate_session_token(l)?;
    let profile_home = prepare_profile(l)?;
    let native_debug_token = new_native_debug_token();
    let ws_res = resolve_ws_port()?;
    let ws_port = ws_res.port;
    // Free the reserved port for PPSSPP to bind before spawning it.
    drop(ws_res);

    let ppsspp_pid = spawn_detached(&emu_spec(l, &profile_home, ws_port, &native_debug_token)?)?;
    // PPSSPP PID를 spawn 직후 즉시 기록한다 — 이후 단계(WS 대기·bridge spawn)가 실패해 종료 처리로
    // 넘어가도, terminate가 혹시 놓친 프로세스를 status.owned_instance가 이 pidfile로 찾아 정리할 수
    // 있게 한다. 성공 시 bridge.pid는 아래에서 추가 기록한다.
    write_pidfile(l.log_path, "ppsspp.pid", ppsspp_pid);
    if let Err(e) = wait_survives(
        ppsspp_pid,
        Duration::from_secs(2),
        "PPSSPP exited during startup (crashed — check the launch log)",
    ) {
        let _ = terminate_detached(ppsspp_pid);
        return Err(e);
    }
    if l.display {
        // Keep the macOS display awake for the HITL window and reap the helper (no-op off macOS);
        // the window dies if the display sleeps, same failure mode as the other GUI adapters.
        super::spawn_display_caffeinate(ppsspp_pid);
    }
    if let Err(e) = wait_ws_ready(ppsspp_pid, ws_port, Duration::from_secs(8)) {
        let _ = terminate_detached(ppsspp_pid);
        return Err(e);
    }
    let bridge = match bridge_spec(l, ws_port, &native_debug_token).emulator_dependency(ppsspp_pid)
    {
        Ok(spec) => spec,
        Err(e) => {
            let _ = terminate_detached(ppsspp_pid);
            return Err(e);
        }
    };
    let bridge_pid = match spawn_detached(&bridge) {
        Ok(pid) => pid,
        Err(e) => {
            let _ = terminate_detached(ppsspp_pid);
            return Err(e);
        }
    };
    // 브리지가 곧바로 죽지 않았는지 확인한다(잘못된 인자·MCP 즉시 연결실패 등). 죽었으면 launch가 성공을
    // 오보하고 ppsspp를 orphan으로 남기지 않도록 둘 다 정리하고 에러를 낸다.
    if let Err(e) = wait_survives(
        bridge_pid,
        Duration::from_secs(2),
        "emucap-ppsspp-bridge exited during startup (couldn't reach the debugger WebSocket or the MCP listener — check the launch log)",
    ) {
        let _ = terminate_detached(bridge_pid);
        let _ = terminate_detached(ppsspp_pid);
        return Err(e);
    }
    write_pidfile(l.log_path, "bridge.pid", bridge_pid);
    Ok(Launched {
        ppsspp_pid,
        bridge_pid,
        ws_port,
    })
}

fn new_native_debug_token() -> String {
    format!(
        "native-{}{}",
        ulid::Ulid::generate().to_string().to_lowercase(),
        ulid::Ulid::generate().to_string().to_lowercase()
    )
}

/// generation pidfile을 RUN_DIR(log_path의 부모)에 기록한다. status가 이걸 읽어
/// 소유 인스턴스 PID를 재발견하므로, agent가 launch 응답을 지나쳐도(다음 턴 등) 자기 것만 kill할 수 있다.
fn write_pidfile(log_path: &Path, name: &str, pid: u32) {
    if let Some(run_dir) = log_path.parent() {
        let _ = std::fs::write(run_dir.join(name), format!("{pid}\n"));
    }
}

#[cfg(test)]
#[path = "ppsspp_tests.rs"]
mod tests;
