//! Flycast (Dreamcast) launch orchestration.
//!
//! Flycast reads its config from a per-OS location: macOS uses `$HOME`, Linux uses XDG, and Windows
//! uses the executable directory. To avoid touching the user's real install we run an emucap-owned
//! runtime copy and seed an emucap-owned config. An operator config is imported only through the
//! explicit `EMUCAP_FLYCAST_CONFIG` input; interpreter, mute, and GDB are set on the private copy.

use super::spec::{flycast_spec, SpecOpts};
use std::path::{Path, PathBuf};

const MAX_IMPORTED_CONFIG_BYTES: u64 = 1024 * 1024;

fn imported_config() -> std::io::Result<String> {
    let Some(raw) = std::env::var_os("EMUCAP_FLYCAST_CONFIG") else {
        return Ok(String::new());
    };
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "EMUCAP_FLYCAST_CONFIG must be an absolute regular file",
        ));
    }
    let bytes =
        crate::path_safety::read_bounded_regular_file_no_follow(&path, MAX_IMPORTED_CONFIG_BYTES)?;
    String::from_utf8(bytes).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("EMUCAP_FLYCAST_CONFIG is not UTF-8: {error}"),
        )
    })
}

/// Resolve the Flycast binary: `FLYCAST_APP` override, else the emucap-owned build output,
/// a legacy `$HOME/flycast/build` build, or the first Flycast executable on PATH.
pub fn build_home() -> std::io::Result<PathBuf> {
    match std::env::var_os("EMUCAP_FLYCAST_BUILD_HOME") {
        Some(path) => Ok(PathBuf::from(path)),
        None => Ok(super::emu_home_base()?.join("flycast-build")),
    }
}

pub fn resolve_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("FLYCAST_APP") {
        let p = PathBuf::from(explicit);
        if super::is_runnable_file(&p) {
            return Some(p);
        }
        if let Some(binary) = app_bundle_executable(&p) {
            if super::is_runnable_file(&binary) {
                return Some(binary);
            }
        }
    }
    let build_home = build_home().ok()?;
    for candidate in [
        "work/build/Flycast.app/Contents/MacOS/Flycast",
        "work/build/flycast",
        "work/build/Flycast.exe",
    ] {
        let p = build_home.join(candidate);
        if super::is_runnable_file(&p) {
            return Some(p);
        }
    }
    if let Some(home) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
    {
        let base = home.join("flycast/build");
        for candidate in ["Flycast.app/Contents/MacOS/Flycast", "flycast"] {
            let p = base.join(candidate);
            if super::is_runnable_file(&p) {
                return Some(p);
            }
        }
    }
    if let Some(default) = super::first_existing_file(default_install_candidates()) {
        return Some(default);
    }
    let exe = if cfg!(windows) {
        "Flycast.exe"
    } else {
        "flycast"
    };
    super::find_on_path(exe)
}

fn app_bundle_executable(path: &Path) -> Option<PathBuf> {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        .then(|| path.join("Contents/MacOS/Flycast"))
}

pub fn default_install_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from(
            "/Applications/Flycast.app/Contents/MacOS/Flycast",
        ));
    }
    #[cfg(windows)]
    {
        for key in [
            "LOCALAPPDATA",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "USERPROFILE",
        ] {
            if let Some(base) = std::env::var_os(key)
                .map(PathBuf::from)
                .filter(|base| base.is_absolute())
            {
                candidates.push(base.join("Programs/Flycast/Flycast.exe"));
                candidates.push(base.join("Flycast/Flycast.exe"));
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(home) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
        {
            candidates.push(home.join(".local/bin/flycast"));
        }
    }
    candidates
}

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub content: &'a str,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<super::RuntimeEnv<'a>>,
    /// Mute audio (default true for debugging).
    pub mute: bool,
    /// Enable Flycast's GDB stub (for the exec-breakpoint path).
    pub gdb: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRuntimeBinary {
    pub binary: PathBuf,
    pub portable_dir: PathBuf,
}

fn app_bundle_root(binary: &Path) -> Option<(&Path, PathBuf)> {
    for ancestor in binary.ancestors() {
        if ancestor
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        {
            let rel = binary.strip_prefix(ancestor).ok()?.to_path_buf();
            return Some((ancestor, rel));
        }
    }
    None
}

/// Run Flycast from an emucap-owned copy. This keeps rebuilds, generation config writes, and Windows'
/// executable-directory config model away from the user's real install.
pub fn prepare_runtime_binary(
    source_binary: &Path,
    iso_home: &Path,
) -> std::io::Result<PreparedRuntimeBinary> {
    let portable_dir = iso_home.join("portable");
    std::fs::create_dir_all(&portable_dir)?;
    if source_binary.starts_with(&portable_dir) {
        if super::has_symlink_component_under(iso_home, source_binary) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "portable Flycast binary path contains a symlink, refusing to launch: {}",
                    source_binary.display()
                ),
            ));
        }
        return Ok(PreparedRuntimeBinary {
            binary: source_binary.to_path_buf(),
            portable_dir,
        });
    }

    let binary = if let Some((app_root, rel)) = app_bundle_root(source_binary) {
        let app_name = app_root.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid Flycast app path: {}", app_root.display()),
            )
        })?;
        let dst_root = portable_dir.join(app_name);
        super::copy_dir_replace(app_root, &dst_root)?;
        dst_root.join(rel)
    } else {
        let exe_name = source_binary.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid Flycast binary path: {}", source_binary.display()),
            )
        })?;
        let binary = portable_dir.join(exe_name);
        super::copy_file_replace(source_binary, &binary)?;
        super::copy_adjacent_dlls(source_binary, &portable_dir)?;
        binary
    };

    Ok(PreparedRuntimeBinary {
        binary,
        portable_dir,
    })
}

/// Seed the isolated config and launch Flycast detached, pointed at the emucap-owned home so the user's
/// real Flycast config is untouched. Returns the child pid.
pub fn launch(l: &Launch) -> std::io::Result<u32> {
    let iso_home = super::emu_generation_dir("flycast", l.port, l.runtime)?;
    let runtime = prepare_runtime_binary(l.binary, &iso_home)?;
    let launch_binary = runtime.binary.clone();

    // Per-OS: where Flycast reads config and which environment redirects it there.
    #[cfg(target_os = "macos")]
    let (iso_cfg_dir, iso_env): (PathBuf, Vec<(String, String)>) = {
        (
            iso_home.join(".flycast"),
            vec![("HOME".into(), iso_home.to_string_lossy().into_owned())],
        )
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let (iso_cfg_dir, iso_env): (PathBuf, Vec<(String, String)>) = {
        (
            iso_home.join("config/flycast"),
            vec![
                (
                    "XDG_CONFIG_HOME".into(),
                    iso_home.join("config").to_string_lossy().into_owned(),
                ),
                (
                    "XDG_DATA_HOME".into(),
                    iso_home.join("data").to_string_lossy().into_owned(),
                ),
            ],
        )
    };
    #[cfg(target_os = "windows")]
    let (iso_cfg_dir, iso_env): (PathBuf, Vec<(String, String)>) =
        { (runtime.portable_dir.clone(), Vec::new()) };

    std::fs::create_dir_all(&iso_cfg_dir)?;
    let iso_cfg = iso_cfg_dir.join("emu.cfg");
    let mut text = imported_config()?;
    if !text.contains("[config]") {
        text = format!("[config]\n{text}");
    }
    set_ini(&mut text, "Dynarec.Enabled", "no");
    set_ini(&mut text, "aica.Volume", if l.mute { "0" } else { "100" });
    set_ini(
        &mut text,
        "Debug.GDBEnabled",
        if l.gdb { "yes" } else { "no" },
    );
    std::fs::write(&iso_cfg, text)?;

    let opts = SpecOpts {
        content: l.content,
        port: l.port,
        name: l.name,
        session_token: l.session_token,
        runtime: l.runtime,
        headless: false,
    };
    let mut spec = flycast_spec(&launch_binary, l.log_path, &opts);
    for (k, v) in iso_env {
        spec = spec.env(k, v);
    }
    let pid = super::spawn_detached(&spec)?;
    // Keep the macOS display awake for the HITL window and reap the helper (no-op off macOS).
    super::spawn_display_caffeinate(pid);
    Ok(pid)
}

/// Set `key = value` under the `[config]` section of an emu.cfg-style ini: replace an existing line or
/// insert right after `[config]`. Operates on the isolated copy only.
fn set_ini(text: &mut String, key: &str, value: &str) {
    let prefix = format!("{key} = ");
    if let Some(start) = text
        .lines()
        .position(|l| l.trim_start().starts_with(&prefix))
    {
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        lines[start] = format!("{key} = {value}");
        *text = lines.join("\n");
        text.push('\n');
    } else if let Some(idx) = text.find("[config]\n") {
        let at = idx + "[config]\n".len();
        text.insert_str(at, &format!("{key} = {value}\n"));
    } else {
        text.push_str(&format!("[config]\n{key} = {value}\n"));
    }
}

#[cfg(test)]
#[path = "flycast_tests.rs"]
mod tests;
