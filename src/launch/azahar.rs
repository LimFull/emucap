//! Direct Azahar libretro host for the Nintendo 3DS backend.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{find_on_path, is_runnable_file, spawn_detached, LaunchSpec, RuntimeEnv};

const LOCK: &str = include_str!("../../adapters/azahar-3ds/upstream.lock");

/// `emucap-azahar-build.json` written by `adapters/azahar-3ds/build.sh`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct BuildIdentity {
    pub version: String,
    pub archive_sha256: String,
    pub patchset_sha256: String,
    pub core: String,
    pub core_sha256: String,
}

#[derive(Debug, Clone)]
pub struct CompatibleCore {
    pub path: PathBuf,
    pub identity: BuildIdentity,
}

fn work_dir(repo_root: &Path) -> PathBuf {
    repo_root.join("adapters/azahar-3ds/work")
}

pub fn resolve_frontend(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_AZAHAR_FRONTEND") {
        let path = PathBuf::from(explicit);
        return is_runnable_file(&path).then_some(path);
    }
    let name = if cfg!(windows) {
        "emucap-azahar.exe"
    } else {
        "emucap-azahar"
    };
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
        }
    }
    candidates.push(repo_root.join("target/release").join(name));
    candidates.push(repo_root.join("target/debug").join(name));
    candidates
        .into_iter()
        .find(|path| is_runnable_file(path))
        .or_else(|| find_on_path(name))
}

fn invalid(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

/// The built core whose identity matches this checkout's pinned source and patch stack.
pub fn require_compatible_core(repo_root: &Path) -> std::io::Result<CompatibleCore> {
    let info = std::env::var_os("EMUCAP_AZAHAR_BUILD_INFO")
        .map(PathBuf::from)
        .unwrap_or_else(|| work_dir(repo_root).join("emucap-azahar-build.json"));
    let identity: BuildIdentity = serde_json::from_slice(&fs::read(&info)?).map_err(|error| {
        invalid(format!(
            "invalid Azahar build identity {}: {error}",
            info.display()
        ))
    })?;
    for (key, actual) in [
        ("AZAHAR_VERSION", identity.version.as_str()),
        ("AZAHAR_ARCHIVE_SHA256", identity.archive_sha256.as_str()),
        ("AZAHAR_PATCHSET_SHA256", identity.patchset_sha256.as_str()),
    ] {
        let expected = lock_value(key)
            .ok_or_else(|| invalid(format!("missing {key} in Azahar upstream.lock")))?;
        if actual != expected {
            return Err(invalid(format!(
                "Azahar build identity {key} mismatch: expected {expected}, got {actual}; rebuild adapters/azahar-3ds"
            )));
        }
    }
    let core = info
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&identity.core);
    if identity.core.contains(['/', '\\']) || !core.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Azahar core not found: {}", core.display()),
        ));
    }
    let actual = sha256_file(&core)?;
    if actual != identity.core_sha256 {
        return Err(invalid(format!(
            "Azahar core digest mismatch: expected {}, got {actual}",
            identity.core_sha256
        )));
    }
    Ok(CompatibleCore {
        path: core,
        identity,
    })
}

pub fn accepts_content_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["3ds", "cci", "cxi", "3dsx"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

pub struct Launch<'a> {
    pub repo_root: &'a Path,
    pub content: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<RuntimeEnv<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub pid: u32,
    pub frontend: PathBuf,
    pub core: PathBuf,
    pub runtime_home: PathBuf,
    pub log: PathBuf,
}

pub fn launch(request: &Launch<'_>) -> std::io::Result<Launched> {
    if !accepts_content_path(request.content) || !request.content.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "3DS content must be an existing decrypted .3ds/.cci/.cxi/.3dsx image: {}",
                request.content.display()
            ),
        ));
    }
    let frontend = resolve_frontend(request.repo_root).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "emucap-azahar frontend not found; build the release binaries",
        )
    })?;
    let bundle = require_compatible_core(request.repo_root)?;
    let runtime_home = super::emu_generation_dir("azahar-3ds", request.port, request.runtime)?;
    // The running core is an immutable copy, so a rebuild cannot change it mid-generation.
    let core_dir = runtime_home.join("core");
    fs::create_dir_all(&core_dir)?;
    let staged_core = core_dir.join(&bundle.identity.core);
    crate::path_safety::atomic_copy_file(&bundle.path, &staged_core)?;
    let mut permissions = fs::metadata(&staged_core)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&staged_core, permissions)?;
    let log = runtime_home.join("azahar.log");
    let mut spec = LaunchSpec::new(&frontend, &log)
        .arg(request.port.to_string())
        .arg(request.content.to_string_lossy())
        .arg(staged_core.to_string_lossy())
        .arg(&bundle.identity.core_sha256)
        .arg(runtime_home.to_string_lossy())
        .arg(&bundle.identity.version)
        .arg(&bundle.identity.patchset_sha256)
        .runtime_env(request.runtime);
    if let Some(name) = request.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = request.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    let pid = spawn_detached(&spec)?;
    Ok(Launched {
        pid,
        frontend,
        core: staged_core,
        runtime_home,
        log,
    })
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn lock_value(key: &str) -> Option<&str> {
    LOCK.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
}

#[cfg(test)]
#[path = "azahar_tests.rs"]
mod tests;
