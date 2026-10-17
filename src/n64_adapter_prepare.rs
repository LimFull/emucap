//! Transactional Mupen64Plus core, ROM, plugin, and callback preparation.

use super::*;

struct PreparationGuard {
    api: Api,
    core_handle: *mut c_void,
    core_started: bool,
    rom_open: bool,
    plugins: Vec<Plugin>,
}

impl PreparationGuard {
    fn new(api: Api, core_handle: *mut c_void) -> Self {
        Self {
            api,
            core_handle,
            core_started: false,
            rom_open: false,
            plugins: Vec::new(),
        }
    }

    fn finish(mut self) -> (Api, *mut c_void, Vec<Plugin>) {
        let api = self.api;
        let core_handle = self.core_handle;
        let plugins = std::mem::take(&mut self.plugins);
        self.core_started = false;
        self.rom_open = false;
        self.core_handle = ptr::null_mut();
        (api, core_handle, plugins)
    }
}

impl Drop for PreparationGuard {
    fn drop(&mut self) {
        for plugin in self.plugins.drain(..).rev() {
            unsafe {
                let _ = (self.api.core_detach_plugin)(plugin.kind);
                let _ = (plugin.shutdown)();
                close_library(plugin.handle);
            }
        }
        if self.rom_open {
            unsafe {
                let _ = (self.api.core_do_command)(M64CMD_ROM_CLOSE, 0, ptr::null_mut());
            }
        }
        if self.core_started {
            unsafe {
                let _ = (self.api.core_shutdown)();
            }
        }
        if !self.core_handle.is_null() {
            unsafe {
                close_library(self.core_handle);
            }
        }
    }
}

impl Mupen64PlusHost {
    pub fn prepare(root: &Path, runtime_home: &Path, rom_path: &Path) -> N64Result<Self> {
        reset_observation_state();
        std::fs::create_dir_all(runtime_home.join("config"))?;
        std::fs::create_dir_all(runtime_home.join("data"))?;
        std::fs::create_dir_all(runtime_home.join("screens"))?;

        let core_path = platform_library(root, "libmupen64plus")?;
        let core_handle = open_library(&core_path)?;
        let api = match unsafe { load_api(core_handle) } {
            Ok(api) => api,
            Err(error) => {
                unsafe { close_library(core_handle) };
                return Err(error);
            }
        };
        control::configure_state_pause(api.debug_set_run_state);
        let mut preparation = PreparationGuard::new(api, core_handle);

        let config = path_cstring(&runtime_home.join("config"))?;
        let data = path_cstring(root)?;
        check_core("CoreStartup", unsafe {
            symbol::<CoreStartup>(core_handle, b"CoreStartup\0")?(
                CORE_API_VERSION,
                config.as_ptr(),
                data.as_ptr(),
                ptr::null_mut(),
                debug_log_callback,
                ptr::null_mut(),
                state_callback,
            )
        })?;
        preparation.core_started = true;
        eprintln!("[mupen64plus-native] core started");

        let mut core_section = ptr::null_mut();
        check_core("ConfigOpenSection(Core)", unsafe {
            (api.config_open_section)(cstr(b"Core\0").as_ptr(), &mut core_section)
        })?;
        set_config_int(&api, core_section, b"R4300Emulator\0", M64TYPE_INT, 0)?;
        set_config_int(&api, core_section, b"EnableDebugger\0", M64TYPE_BOOL, 1)?;
        set_config_int(&api, core_section, b"OnScreenDisplay\0", M64TYPE_BOOL, 0)?;
        set_config_string(
            &api,
            core_section,
            b"ScreenshotPath\0",
            &runtime_home.join("screens"),
        )?;

        let rom = read_rom(rom_path)?;
        let rom_len = c_int::try_from(rom.len())
            .map_err(|_| N64Error::BadParams("N64 ROM is too large".into()))?;
        check_core("ROM_OPEN", unsafe {
            (api.core_do_command)(M64CMD_ROM_OPEN, rom_len, rom.as_ptr() as *mut c_void)
        })?;
        preparation.rom_open = true;
        eprintln!("[mupen64plus-native] ROM opened");

        let display = display_requested();
        let requested_plugins = [
            (M64PLUGIN_GFX, "mupen64plus-video-rice"),
            (M64PLUGIN_INPUT, "mupen64plus-input-sdl"),
            (M64PLUGIN_RSP, "mupen64plus-rsp-hle"),
        ];

        for (kind, stem) in requested_plugins {
            let path = platform_library(root, stem)?;
            eprintln!("[mupen64plus-native] loading plugin {}", path.display());
            let handle = open_library(&path)?;
            eprintln!("[mupen64plus-native] loaded plugin {stem}");
            let startup = match unsafe { symbol::<PluginStartup>(handle, b"PluginStartup\0") } {
                Ok(startup) => startup,
                Err(error) => {
                    unsafe { close_library(handle) };
                    return Err(error);
                }
            };
            let shutdown = match unsafe { symbol::<PluginShutdown>(handle, b"PluginShutdown\0") } {
                Ok(shutdown) => shutdown,
                Err(error) => {
                    unsafe { close_library(handle) };
                    return Err(error);
                }
            };
            if let Err(error) = check_core("PluginStartup", unsafe {
                startup(core_handle, ptr::null_mut(), debug_log_callback)
            }) {
                unsafe { close_library(handle) };
                return Err(error);
            }
            if kind == M64PLUGIN_INPUT {
                if let Err(error) = control::configure_input(&api) {
                    unsafe {
                        let _ = shutdown();
                        close_library(handle);
                    }
                    return Err(error);
                }
            }
            if let Err(error) = check_core("CoreAttachPlugin", unsafe {
                (api.core_attach_plugin)(kind, handle)
            }) {
                unsafe {
                    let _ = shutdown();
                    close_library(handle);
                }
                return Err(error);
            }
            preparation.plugins.push(Plugin {
                kind,
                handle,
                shutdown,
            });
            eprintln!("[mupen64plus-native] attached plugin {stem}");
        }

        check_core("CoreEmucapSetStateBoundaryCallback", unsafe {
            (api.core_set_state_boundary_callback)(1, control::pause_on_state_boundary)
        })?;
        check_core("CoreEmucapSetPollCallback", unsafe {
            (api.core_set_poll_callback)(1, poll::completed)
        })?;
        check_core("DebugSetCallbacks", unsafe {
            (api.debug_set_callbacks)(
                debug_init_callback,
                debug_update_callback,
                debug_vi_callback,
            )
        })?;
        check_core("SET_FRAME_CALLBACK", unsafe {
            (api.core_do_command)(
                M64CMD_SET_FRAME_CALLBACK,
                0,
                frame::frame_callback as *const () as *mut c_void,
            )
        })?;
        eprintln!("[mupen64plus-native] debugger callbacks registered");

        let (api, core_handle, plugins) = preparation.finish();
        frame::set_frame_resume_hook(api.debug_frame_resume);
        Ok(Self {
            api,
            core_handle,
            plugins,
            rom_path: rom_path.to_path_buf(),
            name: std::env::var("EMUCAP_NAME").ok(),
            launch_id: std::env::var("EMUCAP_LAUNCH_ID").ok(),
            build: crate::build_identity::BUILD_HASH.to_string(),
            runtime_home: runtime_home.to_path_buf(),
            display,
            frozen: false,
            frame_paused: false,
            request_cancellation: Default::default(),
            producer_ownership: None,
            frame_clock_synchronized: false,
            controller: Default::default(),
            input_baseline: None,
            parent_input_baseline: None,
            breakpoints: BTreeMap::new(),
            next_breakpoint_id: 1,
            debug_events: VecDeque::new(),
            last_debug_update_seen: 0,
            next_hit_seq: 1,
            next_reset_seq: 1,
            started: false,
        })
    }
}

pub(super) fn set_config_int(
    api: &Api,
    section: *mut c_void,
    name: &'static [u8],
    kind: c_int,
    value: c_int,
) -> N64Result<()> {
    check_core("ConfigSetParameter", unsafe {
        (api.config_set_parameter)(
            section,
            cstr(name).as_ptr(),
            kind,
            &value as *const c_int as *const c_void,
        )
    })
}

pub(super) fn set_config_string(
    api: &Api,
    section: *mut c_void,
    name: &'static [u8],
    value: &Path,
) -> N64Result<()> {
    let value = path_cstring(value)?;
    check_core("ConfigSetParameter", unsafe {
        (api.config_set_parameter)(
            section,
            cstr(name).as_ptr(),
            M64TYPE_STRING,
            value.as_ptr() as *const c_void,
        )
    })
}

// Mupen64Plus 2.6.0 CART_ROM_MAX_SIZE, enforced by M64CMD_ROM_OPEN.
const MAX_ROM_BYTES: u64 = 0x4000000;

fn read_rom(path: &Path) -> N64Result<Vec<u8>> {
    // Explicit ROM paths may be symlinks. Inspect and read the same opened file.
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let length = file.metadata()?.len();
    if length > c_int::MAX as u64 {
        return Err(N64Error::BadParams("N64 ROM is too large".into()));
    }
    crate::path_safety::read_bounded_opened_regular_file(file, MAX_ROM_BYTES).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            // Preserve the native size-rejection result without allocating its input.
            N64Error::Core {
                operation: "ROM_OPEN",
                code: 4,
            }
        } else {
            error.into()
        }
    })
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn rom_admission_bounds_actual_file_before_native_open() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("game.z64");
        std::fs::write(&path, b"rom").unwrap();
        assert_eq!(read_rom(&path).unwrap(), b"rom");
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_ROM_BYTES + 1).unwrap();
        assert!(matches!(
            read_rom(&path),
            Err(N64Error::Core {
                operation: "ROM_OPEN",
                code: 4
            })
        ));
        file.set_len(c_int::MAX as u64 + 1).unwrap();
        assert!(matches!(read_rom(&path), Err(N64Error::BadParams(_))));
        file.set_len(MAX_ROM_BYTES).unwrap();
        assert_eq!(read_rom(&path).unwrap().len() as u64, MAX_ROM_BYTES);
        assert!(read_rom(tmp.path()).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn explicit_rom_symlinks_remain_supported_and_special_files_reject() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("game.z64");
        let link = tmp.path().join("selected.z64");
        std::fs::write(&target, b"explicit ROM").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(read_rom(&link).unwrap(), b"explicit ROM");
        let pipe = tmp.path().join("pipe");
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_rom(&pipe).is_err());
    }
}
