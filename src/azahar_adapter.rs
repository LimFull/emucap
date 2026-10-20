//! Managed Nintendo 3DS backend: a direct host for the pinned Azahar libretro core.
//!
//! The host owns the only `retro_run` loop. Every request is serviced between frames on that
//! thread, so the guest never runs while memory, registers or state are accessed.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use libloading::Library;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::live::protocol::{ProtocolError, Request, Response, PROTOCOL_VERSION};

mod ffi;
mod observation;
mod operations;
mod support;

use ffi::*;
use support::*;

const SYSTEM: &str = "3ds";
const ADAPTER: &str = "azahar-libretro";
const MAX_SYNC_FRAMES: u64 = crate::live::temporal::MAX_SYNC_ADVANCE_COUNT;
const MAX_INPUT_PULSE_FRAMES: u64 = 120;
const MAX_STATE_BYTES: u64 = crate::live::recording_capability::CORE_MAX_RECORDING_STATE_BYTES;
const MAX_STATE_SIDECAR_BYTES: u64 = 96 * 1024;
/// Largest single read or write; larger regions are split by the caller.
const MAX_MEMORY_BYTES: u64 = 0x2_0000;
/// The application's 32-bit virtual address space.
const VIRTUAL_SPACE_SIZE: u64 = 0x1_0000_0000;
/// Bottom-screen geometry in the core's default top-over-bottom layout.
const TOUCH_WIDTH: u64 = 320;
const TOUCH_HEIGHT: u64 = 240;
const METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "read_memory",
    "read_memory_batch",
    "write_memory",
    "get_state",
    "pause",
    "resume",
    "step",
    "run_frames",
    "set_input",
    "press_buttons",
    "touch",
    "screenshot",
    "save_state",
    "load_state",
    "reset",
    "execution_speed",
];
const ACTIVE_EXCEPTIONS: &[&str] = &[
    "azahar.input-hold.port-zero-only",
    "azahar.input-pulse.constraints",
    "azahar.screenshot.frozen-only",
    "azahar.state-save.frozen-only",
    "azahar.state-load.frozen-only",
];

#[derive(Debug, thiserror::Error)]
pub enum AzaharError {
    #[error("{0}")]
    BadParams(String),
    #[error("active native operation")]
    Busy,
    #[error("request cancelled")]
    Cancelled,
    #[error("{0}")]
    BadState(String),
    #[error("unsupported Azahar method: {0}")]
    Unsupported(String),
    #[error("Azahar core error: {0}")]
    Core(String),
    #[error("dynamic library error: {0}")]
    Dynamic(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

type AzaharResult<T> = Result<T, AzaharError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct StateIdentity {
    format: String,
    system: String,
    target_os: String,
    target_arch: String,
    media_sha1: String,
    core_sha256: String,
    upstream_version: String,
    patchset_sha256: String,
    frontend_build: String,
    state_sha256: String,
}

pub struct AzaharHost {
    api: CoreApi,
    content_path: PathBuf,
    content_size: u64,
    content_sha1: String,
    core_sha256: String,
    upstream_version: String,
    patchset_sha256: String,
    frontend_build: String,
    name: Option<String>,
    launch_id: Option<String>,
    runtime_home: PathBuf,
    held_buttons: BTreeSet<String>,
    held_touch: Option<(u64, u64)>,
    frozen: bool,
    video_fresh: bool,
    frame: u64,
    fps: f64,
    loaded: bool,
    core_initialized: bool,
    pacer: observation::FramePacer,
    request_cancellation: crate::live::link::RequestCancellation,
    producer_ownership: Option<crate::live::temporal::owner::ProducerOwnership>,
    control_unverified: bool,
    /// Bumped by every operation that can change guest memory at one frame count.
    boundary_seq: u64,
}

impl AzaharHost {
    pub fn open(
        content_path: &Path,
        core_path: &Path,
        expected_core_sha256: &str,
        runtime_home: &Path,
        upstream_version: &str,
        patchset_sha256: &str,
    ) -> AzaharResult<Self> {
        validate_content(content_path)?;
        let core_sha256 = sha256_file(core_path)?;
        if core_sha256 != expected_core_sha256 {
            return Err(AzaharError::BadParams(format!(
                "Azahar staged core digest mismatch: expected {expected_core_sha256}, got {core_sha256}"
            )));
        }
        let system_dir = runtime_home.join("system");
        let save_dir = runtime_home.join("save");
        fs::create_dir_all(&system_dir)?;
        fs::create_dir_all(&save_dir)?;
        let mut options = HashMap::new();
        for (key, value) in CORE_OPTIONS {
            options.insert(
                (*key).to_string(),
                CString::new(*value).expect("static core option"),
            );
        }
        {
            let mut slot = callback_slot()
                .lock()
                .unwrap_or_else(|value| value.into_inner());
            if slot.is_some() {
                return Err(AzaharError::BadState(
                    "another Azahar core is already active in this process".into(),
                ));
            }
            *slot = Some(CallbackState::new(
                path_cstring(&system_dir)?,
                path_cstring(&save_dir)?,
                options,
            ));
        }
        let api = match unsafe { CoreApi::load(core_path) } {
            Ok(api) => api,
            Err(error) => {
                clear_callback_state();
                return Err(error);
            }
        };
        unsafe {
            (api.set_environment)(environment_callback);
            (api.set_video_refresh)(video_callback);
            (api.set_audio_sample)(audio_sample_callback);
            (api.set_audio_sample_batch)(audio_batch_callback);
            (api.set_input_poll)(input_poll_callback);
            (api.set_input_state)(input_state_callback);
            (api.init)();
        }
        let content_c = path_cstring(content_path)?;
        let game_info = RetroGameInfo {
            path: content_c.as_ptr(),
            data: ptr::null(),
            size: 0,
            meta: ptr::null(),
        };
        if !unsafe { (api.load_game)(&game_info) } {
            unsafe { (api.deinit)() };
            clear_callback_state();
            return Err(AzaharError::Core(
                "retro_load_game failed; the title must be a decrypted 3DS image".into(),
            ));
        }
        let mut av_info = RetroSystemAvInfo::default();
        unsafe { (api.get_system_av_info)(&mut av_info) };
        if !av_info.timing.fps.is_finite() || av_info.timing.fps <= 0.0 {
            unsafe {
                (api.unload_game)();
                (api.deinit)();
            }
            clear_callback_state();
            return Err(AzaharError::Core(format!(
                "invalid Azahar frame rate: {}",
                av_info.timing.fps
            )));
        }
        let content_path = content_path.canonicalize()?;
        Ok(Self {
            api,
            content_size: content_path.metadata()?.len(),
            content_sha1: crate::rom::sha1_of_file(&content_path)?,
            content_path,
            core_sha256,
            upstream_version: upstream_version.to_string(),
            patchset_sha256: patchset_sha256.to_string(),
            frontend_build: crate::build_identity::BUILD_HASH.to_string(),
            name: std::env::var("EMUCAP_NAME").ok(),
            launch_id: std::env::var("EMUCAP_LAUNCH_ID").ok(),
            runtime_home: runtime_home.to_path_buf(),
            held_buttons: BTreeSet::new(),
            held_touch: None,
            frozen: false,
            video_fresh: false,
            frame: 0,
            fps: av_info.timing.fps,
            loaded: true,
            core_initialized: true,
            pacer: observation::FramePacer::new(),
            request_cancellation: Default::default(),
            producer_ownership: None,
            control_unverified: false,
            boundary_seq: 0,
        })
    }

    pub fn is_running(&self) -> bool {
        !self.frozen
    }

    pub fn frame_duration(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.fps.max(1.0))
    }

    pub fn run_scheduled_frame(&mut self) -> AzaharResult<()> {
        if !self.frozen {
            self.run_one_frame()?;
        }
        Ok(())
    }

    pub fn backend_terminal(&self) -> bool {
        self.control_unverified
    }

    pub fn handle_request_cancellable(
        &mut self,
        request: Request,
        token: crate::live::link::RequestCancellation,
    ) -> Response {
        self.request_cancellation = token;
        let response = self.handle_request(request);
        if response
            .error
            .as_ref()
            .is_some_and(|e| e.kind == "emulator_error" || e.kind == "adapter_error")
        {
            self.control_unverified = true;
        }
        self.request_cancellation = Default::default();
        response
    }

    pub fn handle_request(&mut self, request: Request) -> Response {
        let id = request.id;
        let params = &request.params;
        let result = match request.method.as_str() {
            "hello" => self.hello(),
            "status" => self.status(),
            "get_rom_info" => self.get_rom_info(),
            "read_memory" => self.read_memory(params),
            "read_memory_batch" => self.read_memory_batch(params),
            "write_memory" => self.write_memory(params),
            "get_state" => self.get_state(),
            "pause" => self.pause(),
            "resume" => self.resume(),
            "step" => self.step(params),
            "run_frames" => self.run_frames(params),
            "set_input" => self.set_input(params),
            "press_buttons" => self.press_buttons(params),
            "touch" => self.touch(params),
            "screenshot" => self.screenshot(),
            "save_state" => self.save_state(params),
            "load_state" => self.load_state(params),
            "reset" => self.reset(),
            "execution_speed" => self.execution_speed(params),
            other => Err(AzaharError::Unsupported(other.into())),
        };
        match result {
            Ok(result) => Response {
                id,
                ok: true,
                result: Some(result),
                error: None,
            },
            Err(error) => Response {
                id,
                ok: false,
                result: None,
                error: Some(ProtocolError {
                    kind: error_kind(&error).into(),
                    message: error.to_string(),
                }),
            },
        }
    }
}

impl Drop for AzaharHost {
    fn drop(&mut self) {
        set_controls(&BTreeSet::new(), None);
        unsafe {
            if self.loaded {
                (self.api.unload_game)();
                self.loaded = false;
            }
            if self.core_initialized {
                (self.api.deinit)();
                self.core_initialized = false;
            }
        }
        clear_callback_state();
    }
}

#[cfg(test)]
#[path = "azahar_adapter_tests.rs"]
mod tests;

pub mod dispatch;
mod temporal_owner;
