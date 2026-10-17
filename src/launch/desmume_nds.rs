//! DeSmuME (Nintendo DS) launch orchestration. DeSmuME exposes each CPU's state through a GDB stub
//! (`--arm9gdb`/`--arm7gdb`) and a bridge process (`emucap-desmume-nds-bridge`) relays those stubs to
//! emucap on the MCP's port. This spawns two processes: headless DeSmuME and the bridge, mirroring
//! `adapters/desmume-nds/launch.sh` (same as MAME PC-98: an emulator + a GDB bridge).

use super::{
    find_on_path, is_runnable_file, process_alive, spawn_detached, terminate_detached, LaunchSpec,
};
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Resolve the headless desmume-cli binary: `EMUCAP_DESMUME_BIN` override, else the repo-local
/// build-headless output from `adapters/desmume-nds/build.sh`. An explicit override that is not a
/// runnable file resolves to `None` (like `launch.sh`, which uses the override with no fallback).
pub fn resolve_binary(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_DESMUME_BIN") {
        let p = PathBuf::from(explicit);
        return is_runnable_file(&p).then_some(p);
    }
    let local = repo_root
        .join("adapters/desmume-nds/work/src/desmume/src/frontend/posix/build-headless/cli")
        .join(if cfg!(windows) {
            "desmume-cli.exe"
        } else {
            "desmume-cli"
        });
    is_runnable_file(&local).then_some(local)
}

fn bridge_binary_name() -> &'static str {
    if cfg!(windows) {
        "emucap-desmume-nds-bridge.exe"
    } else {
        "emucap-desmume-nds-bridge"
    }
}

/// Resolve the Rust NDS bridge binary: `EMUCAP_NDS_BRIDGE_BIN` override, else the sibling of this
/// executable, `target/release`, `target/debug`, then `PATH` (matching `launch.sh`).
pub fn resolve_bridge(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_NDS_BRIDGE_BIN") {
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
    /// The headless desmume-cli binary.
    pub binary: &'a Path,
    /// The `emucap-desmume-nds-bridge` binary that relays the GDB stubs to emucap.
    pub bridge: &'a Path,
    /// The `.nds` ROM.
    pub content: &'a str,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<super::RuntimeEnv<'a>>,
    /// Open a native DeSmuME window (HITL viewing) by running desmume-cli with EMUCAP_NDS_DISPLAY=1;
    /// false = headless (GDB bridge only, the default). On macOS a caffeinate keeps the display awake
    /// while the window lives.
    pub display: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub desmume_pid: u32,
    pub bridge_pid: u32,
    pub arm9_gdb_port: u16,
    pub arm7_gdb_port: u16,
}

/// A GDB stub port for one CPU. `env_key` override (e.g. `NDS_ARM9_GDB_PORT`) if set, else an
/// OS-assigned free port whose reservation listener is held until just before DeSmuME is spawned.
///
/// Deriving GDB ports from the emucap port (the old `port+1000`/`port+1001`) collides across adjacent
/// sessions: emucap ports are consecutive, so session N's ARM7 (`N+1001`) equals session N+1's ARM9
/// (`(N+1)+1000`). Two concurrent NDS sessions then fight over the same GDB port and DeSmuME fails to
/// open the stub. OS-assigned free ports never collide.
#[derive(Debug)]
struct GdbPort {
    port: u16,
    /// Held reservation for a dynamically-allocated port (`None` for an env override). Dropping it
    /// frees the port for DeSmuME to bind; both ports are reserved simultaneously so they differ.
    _reservation: Option<TcpListener>,
}

fn resolve_gdb_port(env_key: &str) -> io::Result<GdbPort> {
    if let Some(raw) = std::env::var_os(env_key) {
        let value = raw.to_string_lossy();
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            let port: u16 = trimmed.parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{env_key} must be a decimal TCP port, got {trimmed:?}"),
                )
            })?;
            if port == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{env_key} must be in 1..=65535"),
                ));
            }
            return Ok(GdbPort {
                port,
                _reservation: None,
            });
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    Ok(GdbPort {
        port,
        _reservation: Some(listener),
    })
}

/// Confirm a just-spawned process survives a brief startup window — an early exit (desmume-cli
/// crashing on a bad ROM/BIOS/port, or the bridge failing on bad args / an immediate connect
/// failure) means the launch failed even though `spawn_detached` returned a pid. This checks
/// liveness only, not readiness: probing the GDB stubs by connecting would occupy their single
/// client slot before the bridge does, so the bridge's GdbRspClient owns GDB readiness (it retries
/// the stubs for 30s). A process still alive at the end is either connected or still retrying.
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
            "SystemRoot is unavailable for GDB listener ownership verification",
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
            "netstat failed while verifying GDB listener ownership",
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
        "GDB listener ownership verification is unsupported on this platform",
    ))
}

/// Wait until the DeSmuME process itself owns both exact IPv4-loopback GDB listeners. A TCP probe
/// would occupy each stub's single client slot and could disclose the per-launch credential to a
/// process that won the reservation-to-spawn race, so readiness is derived from the OS socket
/// owner table instead.
fn wait_gdb_ready(pid: u32, arm9: u16, arm7: u16, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_alive(pid) {
            return Err(io::Error::other(
                "desmume-cli exited before opening both GDB ports (crashed — check the launch log)",
            ));
        }
        let arm9_owned = process_owns_listening_port(pid, arm9).map_err(|error| {
            io::Error::other(format!(
                "could not verify that desmume-cli owns ARM9 GDB port {arm9}: {error}"
            ))
        })?;
        let arm7_owned = process_owns_listening_port(pid, arm7).map_err(|error| {
            io::Error::other(format!(
                "could not verify that desmume-cli owns ARM7 GDB port {arm7}: {error}"
            ))
        })?;
        if arm9_owned && arm7_owned {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "desmume-cli did not own both GDB ports {arm9}/{arm7} within {timeout:?} (refusing to release the native debugger token to unrelated listeners; check the launch log)"
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Headless DeSmuME with both GDB stubs. Mirrors `launch.sh`:
/// `desmume-cli --arm9gdb <p9> --arm7gdb <p7> --disable-sound <rom>`.
fn emu_spec(l: &Launch, arm9: u16, arm7: u16, native_debug_token: &str) -> LaunchSpec {
    let spec = LaunchSpec::new(l.binary, l.log_path)
        .arg("--arm9gdb")
        .arg(arm9.to_string())
        .arg("--arm7gdb")
        .arg(arm7.to_string())
        .arg("--disable-sound")
        .arg(l.content)
        .env("EMUCAP_NATIVE_DEBUG_TOKEN", native_debug_token);
    // Runtime headless/window switch read by the fork's main.cpp. Default (unset) stays headless.
    if l.display {
        spec.env("EMUCAP_NDS_DISPLAY", "1")
    } else {
        spec
    }
}

fn validate_session_token(l: &Launch) -> io::Result<()> {
    if l.session_token.is_some_and(|token| !token.is_empty()) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "EMUCAP_SESSION_TOKEN is required and must be non-empty for DeSmuME",
        ))
    }
}

/// The bridge that relays the ARM9/ARM7 GDB stubs to emucap on `l.port`. Mirrors `launch.sh`:
/// `emucap-desmume-nds-bridge <port> 127.0.0.1:<arm9> 127.0.0.1:<arm7>` with EMUCAP_* env.
fn bridge_spec(l: &Launch, arm9: u16, arm7: u16, native_debug_token: &str) -> LaunchSpec {
    let mut spec = LaunchSpec::new(l.bridge, l.log_path)
        .arg(l.port.to_string())
        .arg(format!("127.0.0.1:{arm9}"))
        .arg(format!("127.0.0.1:{arm7}"))
        .env("EMUCAP_CONTENT", l.content)
        .env("EMUCAP_NATIVE_DEBUG_TOKEN", native_debug_token);
    if let Some(name) = l.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = l.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    spec = spec.runtime_env(l.runtime);
    // HITL 창 세션임을 브리지에도 알린다 → 기본 resume가 both(ARM7이 입력을 읽으므로)로 바뀐다.
    if l.display {
        spec = spec.env("EMUCAP_NDS_DISPLAY", "1");
    }
    spec
}

/// Spawn headless DeSmuME (both GDB stubs), wait until the stubs accept connections, then spawn the
/// bridge that relays them to emucap on `port`. The two GDB ports are OS-assigned free ports (held
/// reserved until DeSmuME is spawned) so concurrent NDS sessions never collide; if DeSmuME never
/// opens the stubs the launcher fails explicitly and cleans DeSmuME up (rather than an opaque bridge
/// `Connection refused`). If the bridge spawn fails, the launcher terminates DeSmuME.
pub fn launch(l: &Launch) -> io::Result<Launched> {
    validate_session_token(l)?;
    let native_debug_token = new_native_debug_token();
    // Reserve both ports simultaneously (both listeners alive) so ARM9 != ARM7, read the numbers,
    // then drop the reservations to free the ports for DeSmuME to bind.
    let arm9_res = resolve_gdb_port("NDS_ARM9_GDB_PORT")?;
    let arm7_res = resolve_gdb_port("NDS_ARM7_GDB_PORT")?;
    let (arm9, arm7) = (arm9_res.port, arm7_res.port);
    drop(arm9_res);
    drop(arm7_res);

    let desmume_pid = spawn_detached(&emu_spec(l, arm9, arm7, &native_debug_token))?;
    // desmume PID를 spawn 직후 즉시 기록한다 — 이후 단계(gdb 대기·bridge spawn)가 실패해 종료 처리로
    // 넘어가도, terminate가 혹시 놓친 프로세스를 status.owned_instance가 이 pidfile로 찾아 정리할 수 있게 한다
    // (실패 경로의 terminate_detached는 SIGTERM→SIGKILL 에스컬레이션이라 desmume-cli처럼 SIGTERM을 무시해도
    // 실제로 죽는다). 성공 시 bridge.pid는 아래에서 추가 기록한다.
    write_pidfile(l.log_path, "desmume.pid", desmume_pid);
    if let Err(e) = wait_survives(
        desmume_pid,
        Duration::from_secs(2),
        "desmume-cli exited during startup (crashed — check the launch log)",
    ) {
        let _ = terminate_detached(desmume_pid);
        return Err(e);
    }
    if l.display {
        // Keep the macOS display awake for the HITL window and reap the helper (no-op off macOS).
        super::spawn_display_caffeinate(desmume_pid);
    }
    if let Err(e) = wait_gdb_ready(desmume_pid, arm9, arm7, Duration::from_secs(30)) {
        let _ = terminate_detached(desmume_pid);
        return Err(e);
    }
    let bridge =
        match bridge_spec(l, arm9, arm7, &native_debug_token).emulator_dependency(desmume_pid) {
            Ok(spec) => spec,
            Err(e) => {
                let _ = terminate_detached(desmume_pid);
                return Err(e);
            }
        };
    let bridge_pid = match spawn_detached(&bridge) {
        Ok(pid) => pid,
        Err(e) => {
            let _ = terminate_detached(desmume_pid);
            return Err(e);
        }
    };
    // 브리지가 곧바로 죽지 않았는지 확인한다(잘못된 인자·GDB/MCP 즉시 연결실패 등). 죽었으면 launch가 성공을
    // 오보하고 desmume를 orphan으로 남기지 않도록 둘 다 정리하고 에러를 낸다(살아남으면 스텁 재시도 중이거나 연결됨).
    if let Err(e) = wait_survives(
        bridge_pid,
        Duration::from_secs(2),
        "emucap-desmume-nds-bridge exited during startup (couldn't reach the GDB stubs or the MCP listener — check the launch log)",
    ) {
        let _ = terminate_detached(bridge_pid);
        let _ = terminate_detached(desmume_pid);
        return Err(e);
    }
    write_pidfile(l.log_path, "bridge.pid", bridge_pid);
    Ok(Launched {
        desmume_pid,
        bridge_pid,
        arm9_gdb_port: arm9,
        arm7_gdb_port: arm7,
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
/// 소유 인스턴스 PID를 재발견하므로, agent가 launch 응답을 지나쳐도(다음 턴 등) 자기 것만 kill할 수 있다 —
/// broad pkill로 도망쳐 타 세션을 죽이는 사고를 막는 인프라다. best-effort(실패해도 launch는 진행).
fn write_pidfile(log_path: &Path, name: &str, pid: u32) {
    if let Some(run_dir) = log_path.parent() {
        let _ = std::fs::write(run_dir.join(name), format!("{pid}\n"));
    }
}

#[cfg(test)]
#[path = "desmume_nds_tests.rs"]
mod tests;
