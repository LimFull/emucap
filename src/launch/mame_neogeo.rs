//! Isolated MAME launcher for the Neo Geo MVS, AES, CD, and Taito F2 adapters.

use super::{
    find_on_path, first_existing_file, spawn_detached, terminate_detached, LaunchSpec, RuntimeEnv,
};
use std::path::{Path, PathBuf};

const MAX_SOFTWARE_LIST_BYTES: u64 = 16 * 1024 * 1024;

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub bridge: &'a Path,
    pub repo_root: &'a Path,
    pub content: &'a Path,
    /// System BIOS set; Taito F2 boards have none.
    pub bios: Option<&'a Path>,
    pub system: &'a str,
    pub log_path: &'a Path,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<RuntimeEnv<'a>>,
    pub display: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NeoGeoSystem {
    Mvs,
    Aes,
    Cd,
    TaitoF2,
}

impl NeoGeoSystem {
    fn parse(system: &str) -> std::io::Result<Self> {
        match system {
            "neogeo_mvs" => Ok(Self::Mvs),
            "neogeo_aes" => Ok(Self::Aes),
            "neogeo_cd" => Ok(Self::Cd),
            "taitof2" => Ok(Self::TaitoF2),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsupported MAME 68000 system: {system}"),
            )),
        }
    }

    /// Display label and file name of the required BIOS set.
    fn bios(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Mvs => Some(("MVS", "neogeo.zip")),
            Self::Aes => Some(("AES", "aes.zip")),
            Self::Cd => Some(("CDZ", "neocdz.zip")),
            Self::TaitoF2 => None,
        }
    }

    fn driver(self, content: &Path) -> std::io::Result<String> {
        match self {
            Self::Mvs => mvs_driver(content),
            Self::Aes => {
                aes_software_name(content)?;
                Ok("aes".into())
            }
            Self::Cd => {
                crate::cue::validate_graph(content)?;
                Ok("neocdz".into())
            }
            Self::TaitoF2 => taitof2_driver(content),
        }
    }

    fn home_component(self) -> &'static str {
        match self {
            Self::Mvs => "mame-neogeo",
            Self::Aes => "mame-neogeo-aes",
            Self::Cd => "mame-neogeo-cd",
            Self::TaitoF2 => "mame-taitof2",
        }
    }

    fn plugin_profile(self) -> &'static str {
        match self {
            Self::Mvs => "neogeo_mvs",
            Self::Aes => "neogeo_aes",
            Self::Cd => "neogeo_cd",
            Self::TaitoF2 => "taitof2",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub mame_pid: u32,
    pub bridge_pid: u32,
    pub gdb_port: u16,
    pub driver: String,
}

pub fn resolve_binary(repo_root: &Path) -> Option<PathBuf> {
    resolve_binary_in(
        repo_root,
        "EMUCAP_NEOGEO_MAME_BIN",
        "adapters/mame-neogeo/work",
    )
}

/// Taito F2 owns a separate MAME subset build, so a Neo Geo build never satisfies it.
pub fn resolve_taitof2_binary(repo_root: &Path) -> Option<PathBuf> {
    resolve_binary_in(
        repo_root,
        "EMUCAP_TAITOF2_MAME_BIN",
        "adapters/mame-taitof2/work",
    )
}

fn resolve_binary_in(repo_root: &Path, env_key: &str, work: &str) -> Option<PathBuf> {
    for key in [env_key, "MAME_BIN"] {
        if let Some(explicit) = std::env::var_os(key).map(PathBuf::from) {
            if super::is_runnable_file(&explicit) {
                return Some(explicit);
            }
        }
    }
    let name = if cfg!(windows) { "mame.exe" } else { "mame" };
    let local = repo_root.join(work).join(name);
    if super::is_runnable_file(&local) {
        return Some(local);
    }
    if let Some(default) = first_existing_file(super::mame::default_install_candidates()) {
        return Some(default);
    }
    find_on_path("mame")
}

pub fn resolve_bridge(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_NEOGEO_BRIDGE_BIN") {
        let path = PathBuf::from(explicit);
        return super::is_runnable_file(&path).then_some(path);
    }
    let name = if cfg!(windows) {
        "emucap-mame-neogeo-bridge.exe"
    } else {
        "emucap-mame-neogeo-bridge"
    };
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join(name));
        }
    }
    candidates.push(repo_root.join("target/release").join(name));
    candidates.push(repo_root.join("target/debug").join(name));
    candidates
        .into_iter()
        .find(|path| super::is_runnable_file(path))
        .or_else(|| find_on_path(name))
}

pub fn default_bios_candidates(content: &Path, system: &str) -> Vec<PathBuf> {
    let Some((_, bios_name)) = NeoGeoSystem::parse(system)
        .ok()
        .and_then(NeoGeoSystem::bios)
    else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    if content.file_name().and_then(|v| v.to_str()) == Some(bios_name) {
        candidates.push(content.to_path_buf());
    }
    if let Some(parent) = content.parent() {
        candidates.push(parent.join(bios_name));
    }
    for home in [
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("USERPROFILE").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    {
        candidates.push(home.join("mame/roms").join(bios_name));
        candidates.push(home.join(".config/retroarch/system").join(bios_name));
        candidates.push(
            home.join("Library/Application Support/RetroArch/system")
                .join(bios_name),
        );
    }
    candidates
}

pub fn resolve_bios(content: &Path, system: &str) -> Option<PathBuf> {
    let env_key = match NeoGeoSystem::parse(system).ok()? {
        NeoGeoSystem::Mvs => "EMUCAP_NEOGEO_BIOS",
        NeoGeoSystem::Aes => "EMUCAP_NEOGEO_AES_BIOS",
        NeoGeoSystem::Cd => "EMUCAP_NEOGEO_CD_BIOS",
        NeoGeoSystem::TaitoF2 => return None,
    };
    if let Some(explicit) = std::env::var_os(env_key) {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    default_bios_candidates(content, system)
        .into_iter()
        .find(|path| path.is_file())
}

pub fn mvs_driver(content: &Path) -> std::io::Result<String> {
    zip_stem(content, "Neo Geo MVS ROM set", "MAME driver")
}

pub fn taitof2_driver(content: &Path) -> std::io::Result<String> {
    zip_stem(content, "Taito F2 ROM set", "MAME driver")
}

pub fn aes_software_name(content: &Path) -> std::io::Result<String> {
    zip_stem(
        content,
        "Neo Geo AES cartridge software-list set",
        "MAME software",
    )
}

fn zip_stem(content: &Path, media_name: &str, stem_name: &str) -> std::io::Result<String> {
    if !content.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{media_name} not found: {}", content.display()),
        ));
    }
    if !content
        .extension()
        .and_then(|v| v.to_str())
        .is_some_and(|v| v.eq_ignore_ascii_case("zip"))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{media_name} must be a MAME .zip set"),
        ));
    }
    let driver = content
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if driver.is_empty()
        || !driver
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid {stem_name} name from content: {driver:?}"),
        ));
    }
    Ok(driver)
}

pub fn resolve_aes_hash_path(repo_root: &Path) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("EMUCAP_NEOGEO_HASH_PATH") {
        let path = PathBuf::from(explicit);
        return path.join("neogeo.xml").is_file().then_some(path);
    }
    [
        repo_root.join("adapters/mame-neogeo/work/hash"),
        repo_root.join("adapters/mame-neogeo/work/mame-src/hash"),
    ]
    .into_iter()
    .find(|path| path.join("neogeo.xml").is_file())
}

fn validate_aes_software(hash_path: &Path, software: &str) -> std::io::Result<()> {
    let list_path = hash_path.join("neogeo.xml");
    let metadata = std::fs::metadata(&list_path)?;
    if metadata.len() > MAX_SOFTWARE_LIST_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Neo Geo software list exceeds {} bytes: {}",
                MAX_SOFTWARE_LIST_BYTES,
                list_path.display()
            ),
        ));
    }
    let list = std::fs::read_to_string(&list_path)?;
    let entry_start = format!("<software name=\"{software}\"");
    let entry = list
        .split_once(&entry_start)
        .and_then(|(_, rest)| rest.split_once("</software>").map(|(entry, _)| entry))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Neo Geo software {software:?} is absent from {}",
                    list_path.display()
                ),
            )
        })?;
    let aes_compatible = entry.lines().any(|line| {
        let Some((_, after_name)) = line.split_once("name=\"compatibility\"") else {
            return false;
        };
        let Some((_, after_value)) = after_name.split_once("value=\"") else {
            return false;
        };
        let Some((value, _)) = after_value.split_once('"') else {
            return false;
        };
        value.split(',').any(|item| item.trim() == "AES")
    });
    if !aes_compatible {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Neo Geo software {software:?} is not marked AES-compatible in {}",
                list_path.display()
            ),
        ));
    }
    Ok(())
}

fn gdb_port(port: u16) -> std::io::Result<u16> {
    port.checked_add(1000).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("MAME GDB port would overflow for EMUCAP_PORT={port}"),
        )
    })
}

fn rompath(content: &Path, bios: Option<&Path>) -> std::io::Result<String> {
    let content_dir = content.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "MAME content has no parent directory",
        )
    })?;
    // Clone sets resolve their parent set from the same directory.
    let Some(bios) = bios else {
        return Ok(content_dir.to_string_lossy().into_owned());
    };
    let bios_dir = bios.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Neo Geo BIOS has no parent directory",
        )
    })?;
    if content_dir == bios_dir {
        Ok(content_dir.to_string_lossy().into_owned())
    } else {
        Ok(format!(
            "{};{}",
            content_dir.to_string_lossy(),
            bios_dir.to_string_lossy()
        ))
    }
}

pub fn mame_spec(
    launch: &Launch<'_>,
    driver: &str,
    gdb_port: u16,
    native_debug_token: &str,
) -> std::io::Result<LaunchSpec> {
    let system = NeoGeoSystem::parse(launch.system)?;
    if system == NeoGeoSystem::Aes {
        let hash_path = resolve_aes_hash_path(launch.repo_root).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Neo Geo AES software list neogeo.xml not found; build adapters/mame-neogeo or set EMUCAP_NEOGEO_HASH_PATH",
            )
        })?;
        let software = aes_software_name(launch.content)?;
        validate_aes_software(&hash_path, &software)?;
    }
    let home = super::emu_generation_dir(system.home_component(), launch.port, launch.runtime)?;
    mame_spec_in_home(launch, driver, gdb_port, native_debug_token, &home)
}

fn mame_spec_in_home(
    launch: &Launch<'_>,
    driver: &str,
    gdb_port: u16,
    native_debug_token: &str,
    home: &Path,
) -> std::io::Result<LaunchSpec> {
    let system = NeoGeoSystem::parse(launch.system)?;
    let home_text = home.to_string_lossy();
    let pluginspath = launch.repo_root.join("adapters/mame-pc98/plugins");
    let mut args = vec![driver.into(), "-rompath".into()];
    args.push(match system {
        NeoGeoSystem::Mvs | NeoGeoSystem::Aes | NeoGeoSystem::TaitoF2 => {
            rompath(launch.content, launch.bios)?
        }
        NeoGeoSystem::Cd => launch
            .bios
            .and_then(Path::parent)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Neo Geo CD BIOS has no parent directory",
                )
            })?
            .to_string_lossy()
            .into_owned(),
    });
    if system == NeoGeoSystem::Cd {
        args.extend([
            "-bios".into(),
            "official".into(),
            "-cdrom".into(),
            launch.content.to_string_lossy().into_owned(),
        ]);
    } else if system == NeoGeoSystem::Aes {
        let hash_path = resolve_aes_hash_path(launch.repo_root).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Neo Geo AES software list neogeo.xml not found; build adapters/mame-neogeo or set EMUCAP_NEOGEO_HASH_PATH",
            )
        })?;
        let software = aes_software_name(launch.content)?;
        validate_aes_software(&hash_path, &software)?;
        args.extend([
            "-hashpath".into(),
            hash_path.to_string_lossy().into_owned(),
            "-bios".into(),
            "japan".into(),
            "-cart".into(),
            software,
        ]);
    }
    args.extend([
        "-homepath".into(),
        home_text.clone().into_owned(),
        "-cfg_directory".into(),
        format!("{home_text}/cfg"),
        "-nvram_directory".into(),
        format!("{home_text}/nvram"),
        "-input_directory".into(),
        format!("{home_text}/inp"),
        "-state_directory".into(),
        format!("{home_text}/sta"),
        "-snapshot_directory".into(),
        format!("{home_text}/snap"),
        "-diff_directory".into(),
        format!("{home_text}/diff"),
        "-comment_directory".into(),
        format!("{home_text}/comments"),
        "-skip_gameinfo".into(),
        "-debug".into(),
        "-debugger".into(),
        "none".into(),
        "-pluginspath".into(),
        pluginspath.to_string_lossy().into_owned(),
        "-plugins".into(),
        "-plugin".into(),
        "emucap_gdbstub".into(),
        "-noreadconfig".into(),
        "-window".into(),
        "-nomaximize".into(),
        "-sound".into(),
        "none".into(),
    ]);
    if !launch.display {
        // Native Windows MAME has no SDL video-driver option.
        if !cfg!(windows) {
            args.extend(["-videodriver", "dummy"].map(String::from));
        }
        args.extend(
            [
                "-video",
                "none",
                "-keyboardprovider",
                "none",
                "-mouseprovider",
                "none",
                "-output",
                "none",
            ]
            .map(String::from),
        );
    }
    let mut spec = LaunchSpec {
        program: launch.binary.into(),
        args,
        env: Vec::new(),
        log_path: launch.log_path.into(),
        cwd: None,
    }
    .env("MAME_GDB_PORT", gdb_port.to_string())
    .env("EMUCAP_NATIVE_DEBUG_TOKEN", native_debug_token)
    .env("EMUCAP_MAME_PROFILE", system.plugin_profile())
    .env(
        "EMUCAP_CONTENT",
        launch.content.to_string_lossy().into_owned(),
    );
    if !launch.display {
        spec = spec.env("SDL_VIDEODRIVER", "dummy");
    } else {
        // The repo-local MAME binary is a fail-closed wrapper. Authorize its raw binary only for
        // an explicit display=true launch; otherwise it appends -video none after these arguments.
        spec = spec.env("MAME_ALLOW_VISIBLE", "1");
    }
    spec = spec.runtime_env(launch.runtime);
    Ok(spec)
}

#[cfg(test)]
fn bridge_spec(
    launch: &Launch<'_>,
    gdb_port: u16,
    native_debug_token: &str,
) -> std::io::Result<LaunchSpec> {
    let system = NeoGeoSystem::parse(launch.system)?;
    let adapter_home =
        super::emu_generation_dir(system.home_component(), launch.port, launch.runtime)?;
    bridge_spec_in_home(launch, gdb_port, native_debug_token, &adapter_home)
}

fn bridge_spec_in_home(
    launch: &Launch<'_>,
    gdb_port: u16,
    native_debug_token: &str,
    adapter_home: &Path,
) -> std::io::Result<LaunchSpec> {
    let mut spec = LaunchSpec::new(launch.bridge, launch.log_path)
        .arg(launch.port.to_string())
        .arg(launch.system)
        .arg(format!("127.0.0.1:{gdb_port}"))
        .env(
            "EMUCAP_ADAPTER_HOME",
            adapter_home.to_string_lossy().into_owned(),
        )
        .env(
            "EMUCAP_CONTENT",
            launch.content.to_string_lossy().into_owned(),
        )
        .env("EMUCAP_NATIVE_DEBUG_TOKEN", native_debug_token);
    if let Some(name) = launch.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = launch.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    Ok(spec.runtime_env(launch.runtime))
}

pub fn launch(launch: &Launch<'_>) -> std::io::Result<Launched> {
    let system = NeoGeoSystem::parse(launch.system)?;
    let driver = system.driver(launch.content)?;
    if let Some((label, bios_name)) = system.bios() {
        let valid = launch.bios.is_some_and(|bios| {
            bios.file_name().and_then(|v| v.to_str()) == Some(bios_name) && bios.is_file()
        });
        if !valid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Neo Geo {label} BIOS must be an existing {bios_name}: {}",
                    launch.bios.unwrap_or(Path::new("")).display()
                ),
            ));
        }
    }
    let port = gdb_port(launch.port)?;
    let native_debug_token = super::new_native_debug_token();
    let home = super::emu_generation_dir(system.home_component(), launch.port, launch.runtime)?;
    for dir in ["cfg", "nvram", "inp", "sta", "snap", "diff", "comments"] {
        std::fs::create_dir_all(home.join(dir))?;
    }
    let mame_pid = spawn_detached(&mame_spec_in_home(
        launch,
        &driver,
        port,
        &native_debug_token,
        &home,
    )?)?;
    let bridge = match bridge_spec_in_home(launch, port, &native_debug_token, &home)?
        .emulator_dependency(mame_pid)
    {
        Ok(spec) => spec,
        Err(error) => {
            let _ = terminate_detached(mame_pid);
            return Err(error);
        }
    };
    let bridge_pid = match spawn_detached(&bridge) {
        Ok(pid) => pid,
        Err(error) => {
            let _ = terminate_detached(mame_pid);
            return Err(error);
        }
    };
    if launch.display {
        super::spawn_display_caffeinate(mame_pid);
    }
    Ok(Launched {
        mame_pid,
        bridge_pid,
        gdb_port: port,
        driver,
    })
}

#[cfg(test)]
#[path = "mame_neogeo_tests.rs"]
mod tests;
