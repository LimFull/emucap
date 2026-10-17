//! Managed original Xbox launch through the pinned xemu fork.

use std::fs;
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{process_alive, spawn_detached, terminate_detached, LaunchSpec, RuntimeEnv};

mod build;
mod files;

pub use build::{
    build_metadata_path, local_build_candidates, read_build_metadata, require_compatible_build,
    resolve_binary, resolve_bridge, BuildMetadata, REQUIRED_HOST_API,
};
pub use files::{
    identity_for_regular_file, resolve_firmware, validate_firmware_root, FileIdentity,
    FirmwareInventory,
};

#[cfg(test)]
use build::lock_value;
pub(crate) use files::copy_verified;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedGeneration {
    pub clock_shift: u8,
    pub runtime_home: PathBuf,
    pub log: PathBuf,
    pub settings: PathBuf,
    pub screenshots: PathBuf,
    pub mcpx: PathBuf,
    pub flash: PathBuf,
    pub hdd: PathBuf,
    pub eeprom: PathBuf,
    pub dvd: PathBuf,
    pub mcpx_identity: FileIdentity,
    pub flash_identity: FileIdentity,
    pub hdd_template_identity: FileIdentity,
    pub dvd_identity: FileIdentity,
}

struct PreparedGenerationCleanup {
    path: PathBuf,
    armed: bool,
}

impl PreparedGenerationCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PreparedGenerationCleanup {
    fn drop(&mut self) {
        if self.armed {
            #[cfg(windows)]
            clear_readonly_files(&self.path);
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)] // Windows clears FILE_ATTRIBUTE_READONLY here.
fn clear_readonly_files(root: &Path) {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            continue;
        }
        if metadata.is_dir() {
            clear_readonly_files(&path);
        } else if metadata.is_file() {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            let _ = fs::set_permissions(&path, permissions);
        }
    }
}

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub bridge: &'a Path,
    pub content: &'a Path,
    pub firmware: &'a FirmwareInventory,
    pub host_build: &'a BuildMetadata,
    pub port: u16,
    pub name: Option<&'a str>,
    pub session_token: Option<&'a str>,
    pub runtime: Option<RuntimeEnv<'a>>,
    pub display: bool,
    pub sound: bool,
    pub start_frozen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub xemu_pid: u32,
    pub bridge_pid: u32,
    pub qmp_port: u16,
    pub gdb_port: u16,
    pub runtime_home: PathBuf,
    pub log: PathBuf,
    pub settings: PathBuf,
    pub hdd: PathBuf,
    pub eeprom: PathBuf,
    pub dvd: PathBuf,
    pub mcpx_identity: FileIdentity,
    pub flash_identity: FileIdentity,
    pub hdd_template_identity: FileIdentity,
    pub eeprom_initial_identity: FileIdentity,
    pub dvd_identity: FileIdentity,
}

fn validate_content(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Xbox content path must be absolute",
        ));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "iso" | "xiso") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Xbox managed launch accepts raw .iso or .xiso content only",
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Xbox content must be a regular non-symlink file: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn toml_string(path: &Path) -> io::Result<String> {
    serde_json::to_string(&path.to_string_lossy().into_owned())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn prepare_generation(launch: &Launch<'_>) -> io::Result<PreparedGeneration> {
    let runtime_home = super::emu_generation_dir("xemu", launch.port, launch.runtime)?;
    prepare_generation_in_home(launch, runtime_home)
}

fn prepare_generation_in_home(
    launch: &Launch<'_>,
    runtime_home: PathBuf,
) -> io::Result<PreparedGeneration> {
    let candidate = std::env::var("EMUCAP_XEMU_QUALIFICATION_SHIFT")
        .map(Some)
        .or_else(|error| match error {
            std::env::VarError::NotPresent => Ok(None),
            _ => Err(io::Error::new(io::ErrorKind::InvalidInput, error)),
        })?;
    let clock_shift = crate::xemu_bridge::qualification_shift(candidate.as_deref())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    validate_content(launch.content)?;
    let mut cleanup = PreparedGenerationCleanup::new(runtime_home.clone());
    let machine = runtime_home.join("machine");
    let screenshots = runtime_home.join("screenshots");
    crate::private_fs::create_private_dir(&machine)?;
    crate::private_fs::create_private_dir(&screenshots)?;
    let mcpx = machine.join("mcpx.bin");
    let flash = machine.join("flash.bin");
    let hdd = machine.join("hdd.qcow2");
    let eeprom = machine.join("eeprom.bin");
    let media = runtime_home.join("media");
    crate::private_fs::create_private_dir(&media)?;
    let extension = launch
        .content
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("xiso")
        .to_ascii_lowercase();
    let dvd = media.join(format!("boot.{extension}"));
    let dvd_identity = identity_for_regular_file(launch.content)?;
    copy_verified(launch.content, &dvd, &dvd_identity)?;
    copy_verified(&launch.firmware.mcpx, &mcpx, &launch.firmware.mcpx_identity)?;
    copy_verified(
        &launch.firmware.flash,
        &flash,
        &launch.firmware.flash_identity,
    )?;
    copy_verified(
        &launch.firmware.hdd_template,
        &hdd,
        &launch.firmware.hdd_template_identity,
    )?;
    if let (Some(source), Some(identity)) = (
        launch.firmware.eeprom_template.as_deref(),
        launch.firmware.eeprom_template_identity.as_ref(),
    ) {
        copy_verified(source, &eeprom, identity)?;
    }
    let settings = runtime_home.join("xemu.toml");
    let log = runtime_home.join("xemu.log");
    let volume_limit = if launch.sound { 1.0 } else { 0.0 };
    let config = format!(
        "[general]\nshow_welcome = false\n\n[general.updates]\ncheck = false\n\n[audio]\nvolume_limit = {volume_limit:.1}\n\n[input.bindings]\nport1 = \"keyboard\"\n\n[sys.files]\nbootrom_path = {}\nflashrom_path = {}\neeprom_path = {}\nhdd_path = {}\n",
        toml_string(&mcpx)?,
        toml_string(&flash)?,
        toml_string(&eeprom)?,
        toml_string(&hdd)?,
    );
    crate::private_fs::write_private_file(&settings, config.as_bytes())?;
    for path in [&mcpx, &flash, &dvd] {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions)?;
        crate::private_fs::verify_private_file(path)?;
    }
    let prepared = PreparedGeneration {
        clock_shift,
        runtime_home,
        log,
        settings,
        screenshots,
        mcpx,
        flash,
        hdd,
        eeprom,
        dvd,
        mcpx_identity: launch.firmware.mcpx_identity.clone(),
        flash_identity: launch.firmware.flash_identity.clone(),
        hdd_template_identity: launch.firmware.hdd_template_identity.clone(),
        dvd_identity,
    };
    cleanup.disarm();
    Ok(prepared)
}

#[derive(Debug)]
struct BackendPorts {
    qmp_port: u16,
    gdb_port: u16,
    _qmp_reservation: TcpListener,
    _gdb_reservation: TcpListener,
}

fn reserve_backend_ports() -> io::Result<BackendPorts> {
    let qmp = TcpListener::bind("127.0.0.1:0")?;
    let gdb = TcpListener::bind("127.0.0.1:0")?;
    Ok(BackendPorts {
        qmp_port: qmp.local_addr()?.port(),
        gdb_port: gdb.local_addr()?.port(),
        _qmp_reservation: qmp,
        _gdb_reservation: gdb,
    })
}

fn emulator_spec(
    launch: &Launch<'_>,
    prepared: &PreparedGeneration,
    qmp_port: u16,
    gdb_port: u16,
    native_debug_token: &str,
) -> LaunchSpec {
    let mut spec = LaunchSpec::new(launch.binary, &prepared.log)
        .arg("-config_path")
        .arg(prepared.settings.to_string_lossy())
        .arg("-dvd_path")
        .arg(prepared.dvd.to_string_lossy())
        .arg("-gdb")
        .arg(format!("tcp:127.0.0.1:{gdb_port},emucap-auth=xemu-gdb-v1"))
        .arg("-qmp")
        .arg(format!(
            "tcp:127.0.0.1:{qmp_port},server=on,wait=off,emucap-auth=xemu-qmp-v1"
        ))
        .arg("-S")
        .arg("-accel")
        .arg("tcg,thread=single")
        .arg("-rtc")
        .arg("clock=vm")
        .arg("-icount")
        .arg(format!(
            "shift={},align=off,sleep=off",
            prepared.clock_shift
        ))
        .env(
            "EMUCAP_XEMU_SCREEN_ROOT",
            prepared.screenshots.to_string_lossy(),
        )
        .env("EMUCAP_NATIVE_DEBUG_TOKEN", native_debug_token);
    if !launch.display {
        spec = spec.arg("-emucap-hidden");
    }
    spec
}

fn bridge_spec(
    launch: &Launch<'_>,
    prepared: &PreparedGeneration,
    qmp_port: u16,
    gdb_port: u16,
    eeprom_identity: &FileIdentity,
    native_debug_token: &str,
) -> LaunchSpec {
    let mut spec = LaunchSpec::new(launch.bridge, &prepared.log)
        .env("EMUCAP_XEMU_CLOCK_SHIFT", prepared.clock_shift.to_string())
        .arg(launch.port.to_string())
        .arg(format!("127.0.0.1:{qmp_port}"))
        .arg(format!("127.0.0.1:{gdb_port}"))
        .env("EMUCAP_CONTENT", prepared.dvd.to_string_lossy())
        .env(
            "EMUCAP_XEMU_SCREEN_ROOT",
            prepared.screenshots.to_string_lossy(),
        )
        .env(
            "EMUCAP_START_FROZEN",
            if launch.start_frozen { "1" } else { "0" },
        )
        .env("EMUCAP_XEMU_MCPX_SHA256", &prepared.mcpx_identity.sha256)
        .env("EMUCAP_XEMU_FLASH_SHA256", &prepared.flash_identity.sha256)
        .env(
            "EMUCAP_XEMU_HDD_TEMPLATE_SHA256",
            &prepared.hdd_template_identity.sha256,
        )
        .env("EMUCAP_XEMU_EEPROM_INITIAL_SHA256", &eeprom_identity.sha256)
        .env("EMUCAP_XEMU_HDD_PATH", prepared.hdd.to_string_lossy())
        .env("EMUCAP_XEMU_EEPROM_PATH", prepared.eeprom.to_string_lossy())
        .env("EMUCAP_NATIVE_DEBUG_TOKEN", native_debug_token)
        .env("EMUCAP_XEMU_HOST_UPSTREAM", &launch.host_build.upstream)
        .env("EMUCAP_XEMU_HOST_TAG", &launch.host_build.tag)
        .env("EMUCAP_XEMU_HOST_COMMIT", &launch.host_build.commit)
        .env(
            "EMUCAP_XEMU_HOST_API",
            launch.host_build.host_api.to_string(),
        )
        .env(
            "EMUCAP_XEMU_HOST_PATCHSET_SHA256",
            &launch.host_build.patchset_sha256,
        )
        .env(
            "EMUCAP_XEMU_HOST_BINARY_SHA256",
            &launch.host_build.binary_sha256,
        )
        .runtime_env(launch.runtime);
    if let Some(name) = launch.name {
        spec = spec.env("EMUCAP_NAME", name);
    }
    if let Some(token) = launch.session_token {
        spec = spec.env("EMUCAP_SESSION_TOKEN", token);
    }
    spec
}

fn wait_for_eeprom(pid: u32, path: &Path, timeout: Duration) -> io::Result<FileIdentity> {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_alive(pid) {
            return Err(io::Error::other(
                "xemu exited before preparing its generation EEPROM; check the launch log",
            ));
        }
        let secured = crate::private_fs::verify_private_file(path)
            .or_else(|_| crate::private_fs::privatize_file(path));
        match secured.and_then(|()| identity_for_regular_file(path)) {
            Ok(identity) if identity.size == 256 => return Ok(identity),
            Ok(identity) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "xemu generation EEPROM must be 256 bytes, got {}",
                        identity.size
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "xemu did not generate its EEPROM within the startup deadline",
                ));
            }
            Err(error) => return Err(error),
        }
    }
}

fn wait_survives(pid: u32, duration: Duration, message: &str) -> io::Result<()> {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return Err(io::Error::other(message.to_string()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn write_pidfile(runtime_home: &Path, name: &str, pid: u32) {
    let _ = crate::private_fs::write_private_file(
        &runtime_home.join(name),
        format!("{pid}\n").as_bytes(),
    );
}

pub fn launch(launch: &Launch<'_>) -> io::Result<Launched> {
    let prepared = prepare_generation(launch)?;
    let ports = reserve_backend_ports()?;
    let qmp_port = ports.qmp_port;
    let gdb_port = ports.gdb_port;
    let native_debug_token = super::new_native_debug_token();
    drop(ports);
    if launch.display {
        super::wake_display_before_gui_launch();
    }
    let xemu_pid = spawn_detached(&emulator_spec(
        launch,
        &prepared,
        qmp_port,
        gdb_port,
        &native_debug_token,
    ))?;
    write_pidfile(&prepared.runtime_home, "xemu.pid", xemu_pid);
    let eeprom_initial_identity =
        match wait_for_eeprom(xemu_pid, &prepared.eeprom, Duration::from_secs(10)) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = terminate_detached(xemu_pid);
                return Err(error);
            }
        };
    if launch.display {
        super::spawn_display_caffeinate(xemu_pid);
    }
    let bridge = match bridge_spec(
        launch,
        &prepared,
        qmp_port,
        gdb_port,
        &eeprom_initial_identity,
        &native_debug_token,
    )
    .emulator_dependency(xemu_pid)
    {
        Ok(spec) => spec,
        Err(error) => {
            let _ = terminate_detached(xemu_pid);
            return Err(error);
        }
    };
    let bridge_pid = match spawn_detached(&bridge) {
        Ok(pid) => pid,
        Err(error) => {
            let _ = terminate_detached(xemu_pid);
            return Err(error);
        }
    };
    if let Err(error) = wait_survives(
        bridge_pid,
        Duration::from_secs(2),
        "emucap-xemu-bridge exited during startup; check the launch log and pinned host build",
    ) {
        let _ = terminate_detached(bridge_pid);
        let _ = terminate_detached(xemu_pid);
        return Err(error);
    }
    write_pidfile(&prepared.runtime_home, "bridge.pid", bridge_pid);
    Ok(Launched {
        xemu_pid,
        bridge_pid,
        qmp_port,
        gdb_port,
        runtime_home: prepared.runtime_home,
        log: prepared.log,
        settings: prepared.settings,
        hdd: prepared.hdd,
        eeprom: prepared.eeprom,
        dvd: prepared.dvd,
        mcpx_identity: prepared.mcpx_identity,
        flash_identity: prepared.flash_identity,
        hdd_template_identity: prepared.hdd_template_identity,
        eeprom_initial_identity,
        dvd_identity: prepared.dvd_identity,
    })
}

#[cfg(test)]
#[path = "xemu_tests.rs"]
mod tests;
