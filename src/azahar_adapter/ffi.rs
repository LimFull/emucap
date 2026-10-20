//! libretro and emucap debug ABI bindings for the Azahar core.
use super::*;

const DEBUG_API_VERSION: u32 = 2;
pub(super) const REGISTER_COUNT: usize = 17;

const RETRO_DEVICE_JOYPAD: u32 = 1;
const RETRO_DEVICE_ANALOG: u32 = 5;
const RETRO_DEVICE_POINTER: u32 = 6;
const RETRO_DEVICE_INDEX_ANALOG_LEFT: u32 = 0;
const RETRO_DEVICE_ID_POINTER_X: u32 = 0;
const RETRO_DEVICE_ID_POINTER_Y: u32 = 1;
const RETRO_DEVICE_ID_POINTER_PRESSED: u32 = 2;

const RETRO_ENVIRONMENT_GET_CAN_DUPE: u32 = 3;
const RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY: u32 = 9;
const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: u32 = 10;
const RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS: u32 = 11;
const RETRO_ENVIRONMENT_GET_VARIABLE: u32 = 15;
const RETRO_ENVIRONMENT_SET_VARIABLES: u32 = 16;
const RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE: u32 = 17;
const RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY: u32 = 31;
const RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO: u32 = 32;
const RETRO_ENVIRONMENT_SET_CONTROLLER_INFO: u32 = 35;
const RETRO_ENVIRONMENT_SET_GEOMETRY: u32 = 37;
const RETRO_ENVIRONMENT_GET_LANGUAGE: u32 = 39;
const RETRO_ENVIRONMENT_SET_SERIALIZATION_QUIRKS: u32 = 44;
const RETRO_ENVIRONMENT_GET_INPUT_MAX_USERS: u32 = 61;
const RETRO_ENVIRONMENT_EXPERIMENTAL: u32 = 0x10000;
const RETRO_ENVIRONMENT_SET_MEMORY_MAPS: u32 = 36 | RETRO_ENVIRONMENT_EXPERIMENTAL;
const PIXEL_XRGB8888: u32 = 1;

/// Fixed options: the software renderer, the interpreter (which checks breakpoints before every
/// instruction), the default top-over-bottom layout, and touch from the libretro pointer device
/// only. The pointer must not vanish while a touch is held.
pub(super) const CORE_OPTIONS: &[(&str, &str)] = &[
    ("citra_graphics_api", "Software"),
    ("citra_use_cpu_jit", "disabled"),
    ("citra_layout_option", "default"),
    ("citra_swap_screen", "Top"),
    ("citra_enable_touch_touchscreen", "enabled"),
    ("citra_enable_mouse_touchscreen", "disabled"),
    ("citra_enable_touch_pointer_timeout", "disabled"),
    ("citra_analog_function", "c_stick"),
];

/// Public button name and its libretro joypad id. Circle Pad directions are analog deflections.
pub(super) const JOYPAD_BUTTONS: &[(&str, u32)] = &[
    ("b", 0),
    ("y", 1),
    ("select", 2),
    ("start", 3),
    ("up", 4),
    ("down", 5),
    ("left", 6),
    ("right", 7),
    ("a", 8),
    ("x", 9),
    ("l", 10),
    ("r", 11),
    ("zl", 12),
    ("zr", 13),
];
pub(super) const CIRCLE_PAD_BUTTONS: &[&str] =
    &["circle_up", "circle_down", "circle_left", "circle_right"];

#[repr(C)]
struct RetroVariable {
    key: *const c_char,
    value: *const c_char,
}

#[repr(C)]
pub(super) struct RetroGameInfo {
    pub(super) path: *const c_char,
    pub(super) data: *const c_void,
    pub(super) size: usize,
    pub(super) meta: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct RetroGameGeometry {
    pub(super) base_width: u32,
    pub(super) base_height: u32,
    pub(super) max_width: u32,
    pub(super) max_height: u32,
    pub(super) aspect_ratio: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct RetroSystemTiming {
    pub(super) fps: f64,
    pub(super) sample_rate: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct RetroSystemAvInfo {
    pub(super) geometry: RetroGameGeometry,
    pub(super) timing: RetroSystemTiming,
}

/// `EmucapDebug::Point` in the core.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct NativePoint {
    pub(super) id: u64,
    pub(super) kind: u32,
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) pause_on_hit: u32,
}

/// `EmucapDebug::Event` in the core.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct NativeEvent {
    pub(super) sequence: u64,
    pub(super) breakpoint_id: u64,
    pub(super) kind: u32,
    pub(super) address: u32,
    pub(super) size: u32,
    pub(super) value: u32,
    pub(super) paused: u32,
    pub(super) registers: [u32; REGISTER_COUNT],
}

pub(super) const STOP_BREAKPOINT: u32 = 1;
pub(super) const STOP_STEP: u32 = 2;

type EnvironmentCallback = extern "C" fn(u32, *mut c_void) -> bool;
type VideoCallback = extern "C" fn(*const c_void, u32, u32, usize);
type AudioSampleCallback = extern "C" fn(i16, i16);
type AudioBatchCallback = extern "C" fn(*const i16, usize) -> usize;
type InputPollCallback = extern "C" fn();
type InputStateCallback = extern "C" fn(u32, u32, u32, u32) -> i16;

pub(super) struct CoreApi {
    pub(super) set_environment: unsafe extern "C" fn(EnvironmentCallback),
    pub(super) set_video_refresh: unsafe extern "C" fn(VideoCallback),
    pub(super) set_audio_sample: unsafe extern "C" fn(AudioSampleCallback),
    pub(super) set_audio_sample_batch: unsafe extern "C" fn(AudioBatchCallback),
    pub(super) set_input_poll: unsafe extern "C" fn(InputPollCallback),
    pub(super) set_input_state: unsafe extern "C" fn(InputStateCallback),
    pub(super) init: unsafe extern "C" fn(),
    pub(super) deinit: unsafe extern "C" fn(),
    pub(super) load_game: unsafe extern "C" fn(*const RetroGameInfo) -> bool,
    pub(super) unload_game: unsafe extern "C" fn(),
    pub(super) run: unsafe extern "C" fn(),
    pub(super) reset: unsafe extern "C" fn(),
    pub(super) get_system_av_info: unsafe extern "C" fn(*mut RetroSystemAvInfo),
    pub(super) serialize_size: unsafe extern "C" fn() -> usize,
    pub(super) serialize: unsafe extern "C" fn(*mut c_void, usize) -> bool,
    pub(super) unserialize: unsafe extern "C" fn(*const c_void, usize) -> bool,
    pub(super) read_memory: unsafe extern "C" fn(u32, *mut u8, usize) -> i32,
    pub(super) write_memory: unsafe extern "C" fn(u32, *const u8, usize) -> i32,
    pub(super) get_registers: unsafe extern "C" fn(*mut u32, usize) -> i32,
    pub(super) program_id: unsafe extern "C" fn(*mut u64) -> i32,
    pub(super) debug_available: unsafe extern "C" fn() -> i32,
    pub(super) set_point: unsafe extern "C" fn(*const NativePoint) -> i32,
    pub(super) clear_point: unsafe extern "C" fn(u64) -> i32,
    pub(super) clear_points: unsafe extern "C" fn(),
    pub(super) poll_event: unsafe extern "C" fn(*mut NativeEvent) -> i32,
    pub(super) take_dropped: unsafe extern "C" fn() -> u64,
    pub(super) resume: unsafe extern "C" fn() -> i32,
    pub(super) request_step: unsafe extern "C" fn(i32),
    pub(super) stop_requested: unsafe extern "C" fn() -> i32,
    pub(super) take_stop_reason: unsafe extern "C" fn() -> u32,
    pub(super) disassemble: unsafe extern "C" fn(u32, i32, *mut c_char, usize, *mut u32) -> i32,
    _library: Library,
}

impl CoreApi {
    pub(super) unsafe fn load(path: &Path) -> AzaharResult<Self> {
        let library = Library::new(path).map_err(|e| AzaharError::Dynamic(e.to_string()))?;
        macro_rules! symbol {
            ($name:literal) => {{
                *library
                    .get(concat!($name, "\0").as_bytes())
                    .map_err(|error| {
                        AzaharError::Dynamic(format!("{}: {}: {error}", path.display(), $name))
                    })?
            }};
        }
        let version: unsafe extern "C" fn() -> u32 = symbol!("emucap_azahar_debug_api_version");
        if version() != DEBUG_API_VERSION {
            return Err(AzaharError::Core(format!(
                "unsupported Azahar debug API version: expected {DEBUG_API_VERSION}; rebuild adapters/azahar-3ds"
            )));
        }
        Ok(Self {
            set_environment: symbol!("retro_set_environment"),
            set_video_refresh: symbol!("retro_set_video_refresh"),
            set_audio_sample: symbol!("retro_set_audio_sample"),
            set_audio_sample_batch: symbol!("retro_set_audio_sample_batch"),
            set_input_poll: symbol!("retro_set_input_poll"),
            set_input_state: symbol!("retro_set_input_state"),
            init: symbol!("retro_init"),
            deinit: symbol!("retro_deinit"),
            load_game: symbol!("retro_load_game"),
            unload_game: symbol!("retro_unload_game"),
            run: symbol!("retro_run"),
            reset: symbol!("retro_reset"),
            get_system_av_info: symbol!("retro_get_system_av_info"),
            serialize_size: symbol!("retro_serialize_size"),
            serialize: symbol!("retro_serialize"),
            unserialize: symbol!("retro_unserialize"),
            read_memory: symbol!("emucap_azahar_read_memory"),
            write_memory: symbol!("emucap_azahar_write_memory"),
            get_registers: symbol!("emucap_azahar_get_registers"),
            program_id: symbol!("emucap_azahar_program_id"),
            debug_available: symbol!("emucap_azahar_debug_available"),
            set_point: symbol!("emucap_azahar_debug_set_point"),
            clear_point: symbol!("emucap_azahar_debug_clear_point"),
            clear_points: symbol!("emucap_azahar_debug_clear_points"),
            poll_event: symbol!("emucap_azahar_debug_poll_event"),
            take_dropped: symbol!("emucap_azahar_debug_take_dropped"),
            resume: symbol!("emucap_azahar_debug_resume"),
            request_step: symbol!("emucap_azahar_debug_request_step"),
            stop_requested: symbol!("emucap_azahar_debug_stop_requested"),
            take_stop_reason: symbol!("emucap_azahar_debug_take_stop_reason"),
            disassemble: symbol!("emucap_azahar_disassemble"),
            _library: library,
        })
    }
}

#[derive(Default)]
pub(super) struct CapturedFrame {
    pub(super) bytes: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) pitch: usize,
    pub(super) callback_count: u64,
}

pub(super) struct CallbackState {
    system_dir: CString,
    save_dir: CString,
    options: HashMap<String, CString>,
    joypad: BTreeSet<u32>,
    circle_pad: (i16, i16),
    /// libretro pointer coordinates of a held touch.
    pointer: Option<(i16, i16)>,
    pub(super) frame: CapturedFrame,
}

impl CallbackState {
    pub(super) fn new(
        system_dir: CString,
        save_dir: CString,
        options: HashMap<String, CString>,
    ) -> Self {
        Self {
            system_dir,
            save_dir,
            options,
            joypad: BTreeSet::new(),
            circle_pad: (0, 0),
            pointer: None,
            frame: CapturedFrame::default(),
        }
    }
}

static CALLBACK_STATE: OnceLock<Mutex<Option<CallbackState>>> = OnceLock::new();

pub(super) fn callback_slot() -> &'static Mutex<Option<CallbackState>> {
    CALLBACK_STATE.get_or_init(|| Mutex::new(None))
}

fn with_state<T>(f: impl FnOnce(&mut CallbackState) -> T) -> Option<T> {
    callback_slot()
        .lock()
        .unwrap_or_else(|value| value.into_inner())
        .as_mut()
        .map(f)
}

pub(super) fn clear_callback_state() {
    *callback_slot()
        .lock()
        .unwrap_or_else(|value| value.into_inner()) = None;
}

pub(super) extern "C" fn environment_callback(command: u32, data: *mut c_void) -> bool {
    if data.is_null() {
        return false;
    }
    with_state(|state| unsafe {
        match command {
            RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY => {
                *data.cast::<*const c_char>() = state.system_dir.as_ptr();
                true
            }
            RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY => {
                *data.cast::<*const c_char>() = state.save_dir.as_ptr();
                true
            }
            RETRO_ENVIRONMENT_SET_VARIABLES => true,
            RETRO_ENVIRONMENT_GET_VARIABLE => {
                let variable = &mut *data.cast::<RetroVariable>();
                if variable.key.is_null() {
                    return false;
                }
                let key = CStr::from_ptr(variable.key).to_string_lossy();
                match state.options.get(key.as_ref()) {
                    Some(value) => {
                        variable.value = value.as_ptr();
                        true
                    }
                    // Unlisted options keep the core's own defaults.
                    None => {
                        variable.value = ptr::null();
                        false
                    }
                }
            }
            RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE => {
                *data.cast::<bool>() = false;
                true
            }
            RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => *data.cast::<u32>() == PIXEL_XRGB8888,
            // Every frame must carry pixels; a duplicate would leave the capture stale.
            RETRO_ENVIRONMENT_GET_CAN_DUPE => {
                *data.cast::<bool>() = false;
                true
            }
            RETRO_ENVIRONMENT_GET_LANGUAGE => {
                *data.cast::<u32>() = 0;
                true
            }
            RETRO_ENVIRONMENT_GET_INPUT_MAX_USERS => {
                *data.cast::<u32>() = 1;
                true
            }
            RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO
            | RETRO_ENVIRONMENT_SET_GEOMETRY
            | RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS
            | RETRO_ENVIRONMENT_SET_CONTROLLER_INFO
            | RETRO_ENVIRONMENT_SET_SERIALIZATION_QUIRKS
            | RETRO_ENVIRONMENT_SET_MEMORY_MAPS => true,
            _ => false,
        }
    })
    .unwrap_or(false)
}

pub(super) extern "C" fn video_callback(
    data: *const c_void,
    width: u32,
    height: u32,
    pitch: usize,
) {
    with_state(|state| {
        state.frame.callback_count = state.frame.callback_count.saturating_add(1);
        let Some(len) = pitch.checked_mul(height as usize) else {
            return;
        };
        if data.is_null() || width == 0 || len == 0 || len > 64 * 1024 * 1024 {
            state.frame.bytes.clear();
            return;
        }
        let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) };
        state.frame.bytes.clear();
        state.frame.bytes.extend_from_slice(bytes);
        state.frame.width = width;
        state.frame.height = height;
        state.frame.pitch = pitch;
    });
}

pub(super) extern "C" fn audio_sample_callback(_left: i16, _right: i16) {}
pub(super) extern "C" fn audio_batch_callback(_data: *const i16, frames: usize) -> usize {
    frames
}
pub(super) extern "C" fn input_poll_callback() {}

pub(super) extern "C" fn input_state_callback(port: u32, device: u32, index: u32, id: u32) -> i16 {
    if port != 0 {
        return 0;
    }
    with_state(|state| match device {
        RETRO_DEVICE_JOYPAD => i16::from(state.joypad.contains(&id)),
        RETRO_DEVICE_ANALOG if index == RETRO_DEVICE_INDEX_ANALOG_LEFT => match id {
            0 => state.circle_pad.0,
            1 => state.circle_pad.1,
            _ => 0,
        },
        RETRO_DEVICE_POINTER => match (state.pointer, id) {
            (Some((x, _)), RETRO_DEVICE_ID_POINTER_X) => x,
            (Some((_, y)), RETRO_DEVICE_ID_POINTER_Y) => y,
            (Some(_), RETRO_DEVICE_ID_POINTER_PRESSED) => 1,
            _ => 0,
        },
        _ => 0,
    })
    .unwrap_or(0)
}

pub(super) fn callback_count() -> AzaharResult<u64> {
    with_state(|state| state.frame.callback_count)
        .ok_or_else(|| AzaharError::BadState("Azahar callback state is unavailable".into()))
}

/// Applies held buttons and an optional bottom-screen touch for the next polls.
pub(super) fn set_controls(buttons: &BTreeSet<String>, touch: Option<(u64, u64)>) {
    with_state(|state| {
        state.joypad = buttons
            .iter()
            .filter_map(|name| JOYPAD_BUTTONS.iter().find(|(n, _)| n == name))
            .map(|(_, id)| *id)
            .collect();
        let axis = |negative: &str, positive: &str| -> i16 {
            match (buttons.contains(negative), buttons.contains(positive)) {
                (true, false) => -0x7fff,
                (false, true) => 0x7fff,
                _ => 0,
            }
        };
        state.circle_pad = (
            axis("circle_left", "circle_right"),
            axis("circle_up", "circle_down"),
        );
        let (width, height) = match (state.frame.width, state.frame.height) {
            (0, _) | (_, 0) => (400, 480),
            size => size,
        };
        state.pointer = touch.map(|(x, y)| touch_pointer(x, y, width, height));
    });
}

/// libretro pointer coordinates (-0x7fff..=0x7fff across the frame) of a bottom-screen pixel.
/// The default layout centers the 320x240 bottom screen under the top screen; the core maps a
/// pointer back with `(p + 0x7fff) / 0xfffe * size`, so aim at the pixel center.
pub(super) fn touch_pointer(x: u64, y: u64, width: u32, height: u32) -> (i16, i16) {
    let left = (u64::from(width).saturating_sub(TOUCH_WIDTH)) / 2;
    let top = u64::from(height).saturating_sub(TOUCH_HEIGHT);
    let axis = |pixel: u64, size: u32| -> i16 {
        let scaled = ((pixel as f64 + 0.5) * 65534.0 / f64::from(size)).round() as i64 - 0x7fff;
        scaled.clamp(-0x7fff, 0x7fff) as i16
    };
    (axis(left + x, width), axis(top + y, height))
}

pub(super) fn controls_released() -> bool {
    with_state(|state| {
        state.joypad.is_empty() && state.circle_pad == (0, 0) && state.pointer.is_none()
    })
    .unwrap_or(false)
}

pub(super) fn captured_png() -> AzaharResult<(Vec<u8>, u32, u32)> {
    let slot = callback_slot()
        .lock()
        .unwrap_or_else(|value| value.into_inner());
    let frame = &slot
        .as_ref()
        .ok_or_else(|| AzaharError::BadState("Azahar callback state is unavailable".into()))?
        .frame;
    let row_bytes = frame.width as usize * 4;
    if frame.bytes.is_empty()
        || row_bytes > frame.pitch
        || frame.pitch.checked_mul(frame.height as usize) != Some(frame.bytes.len())
    {
        return Err(AzaharError::BadState(
            "Azahar has not produced a capturable video frame".into(),
        ));
    }
    let mut rgb = Vec::with_capacity(frame.width as usize * frame.height as usize * 3);
    for row in frame.bytes.chunks(frame.pitch) {
        for pixel in row[..row_bytes].as_chunks::<4>().0 {
            let value = u32::from_ne_bytes(*pixel);
            rgb.extend([(value >> 16) as u8, (value >> 8) as u8, value as u8]);
        }
    }
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| AzaharError::Core(format!("write PNG header: {error}")))?;
        writer
            .write_image_data(&rgb)
            .map_err(|error| AzaharError::Core(format!("write PNG data: {error}")))?;
    }
    Ok((output, frame.width, frame.height))
}
