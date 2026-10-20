use super::*;
use std::io::Read;

/// Title formats the libretro core loads directly. CIA files need installation and are excluded.
const CONTENT_EXTENSIONS: &[&str] = &["3ds", "cci", "cxi", "3dsx"];

pub(super) fn input_buttons() -> Vec<&'static str> {
    JOYPAD_BUTTONS
        .iter()
        .map(|(name, _)| *name)
        .chain(CIRCLE_PAD_BUTTONS.iter().copied())
        .collect()
}

pub(super) fn validate_content(path: &Path) -> AzaharResult<()> {
    if !path.is_file() {
        return Err(AzaharError::BadParams(format!(
            "3DS content not found: {}",
            path.display()
        )));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !CONTENT_EXTENSIONS.contains(&extension.as_str()) {
        return Err(AzaharError::BadParams(format!(
            "the Azahar backend accepts decrypted {} images",
            CONTENT_EXTENSIONS.join("/")
        )));
    }
    Ok(())
}

pub(super) fn normalize_buttons(value: Option<&Value>) -> AzaharResult<BTreeSet<String>> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    let names = value
        .as_array()
        .ok_or_else(|| AzaharError::BadParams("buttons must be a list".into()))?;
    let known = input_buttons();
    names
        .iter()
        .map(|name| {
            let name = name
                .as_str()
                .ok_or_else(|| AzaharError::BadParams("button names must be strings".into()))?
                .trim()
                .to_ascii_lowercase();
            let name = match name.as_str() {
                "enter" | "return" => "start".to_string(),
                _ => name,
            };
            if known.contains(&name.as_str()) {
                Ok(name)
            } else {
                Err(AzaharError::BadParams(format!(
                    "unsupported 3DS button: {name}"
                )))
            }
        })
        .collect()
}

pub(super) fn require_port_zero(params: &Value) -> AzaharResult<()> {
    match optional_num(params, "port")? {
        None | Some(0) => Ok(()),
        Some(port) => Err(AzaharError::BadParams(format!(
            "3DS input supports port 0 only, got {port}"
        ))),
    }
}

pub(super) fn require_frame_unit(params: &Value) -> AzaharResult<()> {
    match params.get("unit").and_then(Value::as_str) {
        None | Some("frames" | "frame") => Ok(()),
        Some(unit) => Err(AzaharError::BadParams(format!(
            "3DS execution control supports frame units only, got {unit}"
        ))),
    }
}

pub(super) fn frame_count(params: &Value) -> AzaharResult<u64> {
    let count = optional_num(params, "n")?
        .or(optional_num(params, "count")?)
        .or(optional_num(params, "frames")?)
        .unwrap_or(1);
    if (1..=MAX_SYNC_FRAMES).contains(&count) {
        Ok(count)
    } else {
        Err(AzaharError::BadParams(format!(
            "frame count must be in 1..={MAX_SYNC_FRAMES}, got {count}"
        )))
    }
}

fn parse_num(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => {
            let text = text.trim();
            match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                Some(hex) => u64::from_str_radix(hex, 16).ok(),
                None => text.parse().ok(),
            }
        }
        _ => None,
    }
}

pub(super) fn optional_num(params: &Value, key: &str) -> AzaharResult<Option<u64>> {
    match params.get(key) {
        Some(value) => parse_num(value)
            .map(Some)
            .ok_or_else(|| AzaharError::BadParams(format!("invalid numeric param: {key}"))),
        None => Ok(None),
    }
}

pub(super) fn required_num(params: &Value, key: &str) -> AzaharResult<u64> {
    optional_num(params, key)?
        .ok_or_else(|| AzaharError::BadParams(format!("missing required param: {key}")))
}

/// Absolute virtual address of `memory_type` + `address`, bounded by `length`.
pub(super) fn virtual_address(params: &Value, length: u64) -> AzaharResult<u32> {
    match params.get("memory_type").and_then(Value::as_str) {
        None | Some("virtual") => {}
        Some(other) => {
            return Err(AzaharError::BadParams(format!(
                "unsupported 3DS memory_type: {other}; this profile supports virtual"
            )))
        }
    }
    let address = required_num(params, "address")?;
    if length == 0 || length > MAX_MEMORY_BYTES {
        return Err(AzaharError::BadParams(format!(
            "length must be in 1..={MAX_MEMORY_BYTES:#x}"
        )));
    }
    if !matches!(address.checked_add(length), Some(end) if end <= VIRTUAL_SPACE_SIZE) {
        return Err(AzaharError::BadParams(format!(
            "virtual access out of range: {address:#x}+{length:#x}"
        )));
    }
    Ok(address as u32)
}

pub(super) fn absolute_path_param(params: &Value, key: &str) -> AzaharResult<PathBuf> {
    let raw = params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AzaharError::BadParams(format!("missing required param: {key}")))?;
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(AzaharError::BadParams(format!(
            "{key} must be an absolute path"
        )));
    }
    Ok(path)
}

pub(super) fn path_cstring(path: &Path) -> AzaharResult<CString> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| AzaharError::BadParams(format!("path contains NUL: {}", path.display())))
}

pub(super) fn sha256_file(path: &Path) -> AzaharResult<String> {
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

pub(super) fn state_sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".emucap.json");
    PathBuf::from(name)
}

pub(super) fn error_kind(error: &AzaharError) -> &'static str {
    match error {
        AzaharError::Busy => "busy",
        AzaharError::Cancelled => "cancelled",
        AzaharError::BadParams(_) => "bad_params",
        AzaharError::BadState(_) => "bad_state",
        AzaharError::Unsupported(_) => "unsupported",
        AzaharError::Core(_) => "emulator_error",
        AzaharError::Dynamic(_) | AzaharError::Io(_) | AzaharError::Json(_) => "adapter_error",
    }
}
