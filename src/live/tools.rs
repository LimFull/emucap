use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use super::link::{EmulatorLink, LinkError};
use super::temporal::finish_with_cleanup;
#[cfg(test)]
pub(crate) use super::tools_state::save_state_in_store;
pub use super::tools_state::{save_state, save_state_for_recording, save_state_with_key};

mod cancellable;
mod controller;
pub use cancellable::{
    step_with_cancellation, tap_axes_in_unit_with_cancellation, tap_axes_with_cancellation,
    tap_with_cancellation,
};
pub use controller::{tap_axes, tap_axes_in_unit};

mod dump;
pub use dump::dump_memory;

mod screenshot;
pub use screenshot::screenshot;

mod pointer;
pub use pointer::{click_pointer, drag_pointer, move_pointer};

#[path = "tools/probe.rs"]
mod probe_operation;
pub use probe_operation::{probe, probe_available, probe_available_for_methods};

#[derive(Debug, PartialEq)]
pub enum ToolOutput {
    Json(Value),
    Image {
        png_base64: String,
        saved_path: Option<String>,
        provenance: Value,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepUnit {
    #[default]
    Frames,
    Instructions,
    InputPolls,
}

impl StepUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Frames => "frames",
            Self::Instructions => "instructions",
            Self::InputPolls => "input_polls",
        }
    }

    pub fn capability_method(self) -> &'static str {
        match self {
            Self::Frames => "step",
            Self::Instructions => "step_instructions",
            Self::InputPolls => "step_input_polls",
        }
    }
}

pub fn read_memory(
    link: &mut dyn EmulatorLink,
    memory_type: &str,
    address: u64,
    length: u64,
) -> Result<ToolOutput, LinkError> {
    let params = json!({ "memory_type": memory_type, "address": address, "length": length });
    Ok(ToolOutput::Json(link.call("read_memory", params)?))
}

/// 에뮬 메모리 영역을 어댑터 내부에서 스캔해 바이트열(hex) 패턴의 매칭 오프셋들만 회신한다.
/// 128KB를 와이어로 안 보내고 오프셋만 돌려줘 토큰·지연을 최소화한다(런타임 문자열/버퍼/테이블 특정).
pub fn find_pattern(
    link: &mut dyn EmulatorLink,
    memory_type: &str,
    hex: &str,
    start: u64,
    length: Option<u64>,
    max_matches: u64,
    align: u64,
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({
        "memory_type": memory_type, "hex": hex,
        "start": start, "max_matches": max_matches, "align": align,
    });
    if let Some(l) = length {
        params["length"] = json!(l);
    }
    Ok(ToolOutput::Json(link.call("find_pattern", params)?))
}

/// Resolve an optional CPU name against the generation-bound target catalog and put the canonical
/// id on the wire. Legacy third-party adapters without a catalog retain pass-through behavior.
fn with_cpu(
    link: &dyn EmulatorLink,
    params: &mut serde_json::Value,
    cpu: Option<&str>,
) -> Result<Option<String>, LinkError> {
    let selected =
        crate::debug_selection::resolve_cpu_target(&link.capabilities().identity.cpu_targets, cpu)
            .map_err(bad_debug_parameter)?;
    if let (Some(cpu), Some(obj)) = (selected.as_deref(), params.as_object_mut()) {
        obj.insert("cpu".into(), json!(cpu));
    }
    Ok(selected)
}

pub fn get_state(
    link: &mut dyn EmulatorLink,
    groups: &[String],
    cpu: Option<&str>,
) -> Result<ToolOutput, LinkError> {
    let advertised_groups = link.capabilities().identity.state_groups.clone();
    let resolved_groups = if groups.is_empty() || advertised_groups.is_empty() {
        Vec::new()
    } else {
        crate::debug_selection::resolve_state_groups(groups, &advertised_groups)
            .map_err(bad_debug_parameter)?
    };
    let mut params = if resolved_groups.is_empty() {
        json!({})
    } else {
        json!({ "groups": resolved_groups })
    };
    with_cpu(link, &mut params, cpu)?;
    let response = link.call("get_state", params)?;
    let response =
        crate::debug_selection::project_state_groups(response, groups, &advertised_groups)
            .map_err(|message| {
                if message.starts_with("unknown state group")
                    || message.starts_with("groups entries")
                {
                    bad_debug_parameter(message)
                } else {
                    LinkError::Protocol(format!("invalid get_state projection: {message}"))
                }
            })?;
    Ok(ToolOutput::Json(response))
}

fn bad_debug_parameter(message: String) -> LinkError {
    LinkError::Emulator {
        kind: "bad_params".into(),
        message,
    }
}

/// Saturn VDP2 비디오 상태를 per-NBG로 디코드해 반환한다(어댑터가 RawRegs를 렌더러 공식으로 디코드).
/// Saturn 전용 — 어댑터가 미지원 시 에러를 반환한다(가용성은 status.methods로 확인).
pub fn get_video_state(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("get_video_state", json!({}))?))
}

/// Saturn 화면좌표(NBGn, x, y) → 그 셀의 char 데이터 베이스 주소를 per-tile로 풀어 반환한다(어댑터가
/// 스크롤 가산·PLSZ 랩·PNT 엔트리 읽기·supplement→charno를 렌더러 권위 공식으로 접는다). 중간값
/// (nt_addr·raw PND·charno·cellbytes·palno·flip) 동봉. Saturn 전용 — 가용성은 status.methods로 확인.
pub fn resolve_tile(
    link: &mut dyn EmulatorLink,
    nbg: u32,
    x: u32,
    y: u32,
) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call(
        "resolve_tile",
        json!({ "nbg": nbg, "x": x, "y": y }),
    )?))
}

/// Mednafen 내장 레이어 enable 마스크를 토글한다(비파괴 VDP1/VDP2 라우팅 확정·클린플레이트용). 어댑터가
/// MDFNGameInfo->LayerNames를 파싱해 이름↔비트를 매핑하고, layers(이름 배열, 대소문자 무시 → 그것만 enable·
/// 나머지 disable) 또는 mask(raw)로 마스크를 조립해 적용한다. 둘 다 생략 시 변경 없이 조회만. PSX 등
/// LayerNames 없는 시스템은 미지원(가용성은 status.methods로 확인). 반환 {layer_names, mask, enabled}.
pub fn set_layer_enable(
    link: &mut dyn EmulatorLink,
    layers: &[String],
    mask: Option<u64>,
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({});
    if !layers.is_empty() {
        params["layers"] = json!(layers);
    }
    if let Some(m) = mask {
        params["mask"] = json!(m);
    }
    Ok(ToolOutput::Json(link.call("set_layer_enable", params)?))
}

pub fn get_rom_info(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("get_rom_info", json!({}))?))
}

pub fn change_media(
    link: &mut dyn EmulatorLink,
    device: &str,
    path: Option<&str>,
    eject: bool,
    expected_sha1: Option<&str>,
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({ "device": device, "eject": eject });
    if let Some(path) = path {
        params["path"] = json!(path);
    }
    if let Some(expected_sha1) = expected_sha1 {
        params["expected_sha1"] = json!(expected_sha1);
    }
    Ok(ToolOutput::Json(link.call("change_media", params)?))
}

pub fn status(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    let mut value = link.call("status", json!({}))?;
    if super::snapshot::supported(link)
        && link.endpoint_port().is_some()
        && link.continuity().runtime_binding.state == super::continuity::RuntimeBindingState::Bound
    {
        value["snapshot_capability"] = json!({"kind":"instruction_snapshot", "snapshot_key_required":true,
            "max_snapshot_bytes":crate::bundle::snapshot::MAX_SNAPSHOT_BYTES,
            "max_host_ms":super::snapshot::MAX_HOST_MS, "boundary":"instruction_boundary",
            "verify_tool":"snapshot_receipt", "recording_required":false});
    }
    Ok(ToolOutput::Json(value))
}

pub fn dismiss_failure(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("dismiss_failure", json!({}))?))
}

/// A single public memory write stays within the conservative 16 KiB chunk used by the PC-98
/// bridge, keeping its hex-encoded NDJSON request bounded. Larger changes compose while frozen.
pub const MAX_WRITE_BYTES: usize = 0x4_000;

pub fn write_memory(
    link: &mut dyn EmulatorLink,
    memory_type: &str,
    address: u64,
    hex: &str,
) -> Result<ToolOutput, LinkError> {
    if hex.is_empty() {
        return Err(bad_params("hex must contain at least one byte"));
    }
    if hex.len() & 1 != 0 {
        return Err(bad_params("hex must have even length"));
    }
    let bytes = hex::decode(hex).map_err(|_| bad_params("hex decode failed"))?;
    write_memory_bytes(link, memory_type, address, &bytes)
}

pub fn write_memory_bytes(
    link: &mut dyn EmulatorLink,
    memory_type: &str,
    address: u64,
    bytes: &[u8],
) -> Result<ToolOutput, LinkError> {
    if bytes.is_empty() {
        return Err(bad_params(
            "write_memory input must contain at least one byte",
        ));
    }
    if bytes.len() > MAX_WRITE_BYTES {
        return Err(bad_params(format!(
            "write_memory input length {:#x} exceeds the {MAX_WRITE_BYTES:#x} byte cap",
            bytes.len()
        )));
    }
    address
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| bad_params("write_memory address+length overflows"))?;
    let params = json!({
        "memory_type": memory_type,
        "address": address,
        "hex": hex::encode(bytes),
    });
    Ok(ToolOutput::Json(link.call("write_memory", params)?))
}

fn bad_params(message: impl Into<String>) -> LinkError {
    LinkError::Emulator {
        kind: "bad_params".into(),
        message: message.into(),
    }
}

fn validate_sync_advance(label: &str, count: u64) -> Result<(), LinkError> {
    if count > super::temporal::MAX_SYNC_ADVANCE_COUNT {
        return Err(bad_params(format!(
            "{label} count {count} exceeds the synchronous cap {}; split the advance and verify each terminal response",
            super::temporal::MAX_SYNC_ADVANCE_COUNT
        )));
    }
    Ok(())
}

fn validate_positive_sync_advance(label: &str, count: u64) -> Result<(), LinkError> {
    if count == 0 {
        return Err(bad_params(format!("{label} count must be at least 1")));
    }
    validate_sync_advance(label, count)
}

pub fn set_input(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
) -> Result<ToolOutput, LinkError> {
    set_controller_state(link, port, buttons, &BTreeMap::new())
}

pub fn set_controller_state(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    axes: &BTreeMap<String, i64>,
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({ "port": port, "buttons": buttons });
    if !axes.is_empty() {
        params
            .as_object_mut()
            .expect("controller state params are an object")
            .insert("axes".into(), json!(axes));
    }
    Ok(ToolOutput::Json(link.call("set_input", params)?))
}

/// 버튼을 frames만큼 실시간으로 누르고 뗀다. raw press_buttons는 지연 명령이라 frozen에선 프레임이 안 흘러
/// no-op이던 것을, 어댑터 press_buttons 핸들러가 g_frozen=false로 *원자적으로* resume해 해결한다(run_frames와
/// 동일). 별도 ensure_running(resume)은 명령 도착 전 free-run으로 watch/BP를 조기 소진시키는 레이스라 쓰지
/// 않는다. frozen을 유지하며 결정론적 단발이 필요하면 tap을 쓴다.
pub fn press_buttons(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    frames: u64,
) -> Result<ToolOutput, LinkError> {
    validate_sync_advance("press_buttons frame", frames)?;
    let params = json!({ "port": port, "buttons": buttons, "frames": frames });
    Ok(ToolOutput::Json(link.call("press_buttons", params)?))
}

/// 하단 터치스크린(256×192)을 (x,y)에서 터치한다 — release면 뗀다, frames>0이면 그만큼 누른 뒤 자동으로 뗀다(탭),
/// 둘 다 없으면 다음 touch까지 hold. 터치스크린이 있는 시스템에서 status.methods에
/// touch가 있을 때만 어댑터로 전달한다.
pub fn touch(
    link: &mut dyn EmulatorLink,
    port: u64,
    x: Option<u64>,
    y: Option<u64>,
    frames: Option<u64>,
    release: bool,
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({ "port": port });
    if release {
        params["release"] = json!(true);
    } else {
        if let Some(x) = x {
            params["x"] = json!(x);
        }
        if let Some(y) = y {
            params["y"] = json!(y);
        }
        if let Some(f) = frames {
            params["frames"] = json!(f);
        }
    }
    Ok(ToolOutput::Json(link.call("touch", params)?))
}

/// 한 번의 frozen 탭: set_input→step(press_frames)→정지 상태에서 해제.
fn one_tap(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    press_frames: u64,
    expected_launch_id: Option<&str>,
) -> Result<Option<(Value, &'static str)>, LinkError> {
    if let Err(primary) = link.call("set_input", json!({ "port": port, "buttons": buttons })) {
        // A lost set_input response is ambiguous: the adapter may already own the override. Always
        // issue an explicit release before returning the original error.
        return finish_transient_input(link, port, expected_launch_id, Err(primary));
    }
    let outcome = link.call("step", json!({ "frames": press_frames.max(1) }));
    let press = finish_transient_input(link, port, expected_launch_id, outcome)?;
    if press.get("status").and_then(Value::as_str) == Some("interrupted") {
        return Ok(Some((press, "press")));
    }
    Ok(None)
}

fn combine_temporal_cleanup_error(primary: Option<LinkError>, cleanup: LinkError) -> LinkError {
    let message = match primary {
        Some(primary) => format!("{primary}; request-scoped cleanup also failed: {cleanup}"),
        None => format!("request-scoped cleanup failed: {cleanup}"),
    };
    LinkError::Emulator {
        kind: "cleanup_failed".into(),
        message,
    }
}

const SESSION_CLEANUP_ATTEMPTS: usize = 3;

/// Deliver an idempotent cleanup call across a replacement front-side session when the first
/// response is ambiguous. The retry is enabled only for links that explicitly preserve the same
/// control session across reconnect; links without that support remain single-call.
///
/// Each direct-link attempt either adopts a pending client or waits at most one 250 ms preaccept
/// slice. Three attempts cover Mesen's worst-case 500 ms reconnect throttle without multiplying a
/// backend request timeout across a long retry train.
pub fn call_session_cleanup(
    link: &mut dyn EmulatorLink,
    method: &str,
    params: Value,
    expected_launch_id: Option<&str>,
) -> Result<Value, LinkError> {
    let reconnectable = link.supports_session_reconnect();
    let current_launch_id = link.capabilities().identity.launch_id.as_deref();
    let current_connection_known = link.capabilities().protocol_version != 0;
    let must_preflight = match expected_launch_id {
        Some(expected) if current_launch_id == Some(expected) => false,
        Some(_) if current_launch_id.is_some() => {
            return Err(LinkError::Emulator {
                kind: "bad_state".into(),
                message: "launch generation changed before session cleanup".into(),
            })
        }
        Some(_) if reconnectable => true,
        Some(_) => {
            return Err(LinkError::Emulator {
                kind: "bad_state".into(),
                message: "launch generation is unavailable before session cleanup".into(),
            })
        }
        None if reconnectable && !current_connection_known => {
            return Err(LinkError::Emulator {
                kind: "bad_state".into(),
                message: "launch generation is unavailable before reconnecting session cleanup"
                    .into(),
            })
        }
        None => false,
    };

    let mut last = LinkError::NotConnected;
    if !must_preflight {
        match link.call(method, params.clone()) {
            Ok(value) => return Ok(value),
            Err(error @ (LinkError::NotConnected | LinkError::Timeout))
                if reconnectable && expected_launch_id.is_some() =>
            {
                last = error;
            }
            Err(error) => return Err(error),
        }
    }
    let Some(expected_launch_id) = expected_launch_id else {
        return Err(last);
    };

    for _ in 0..SESSION_CLEANUP_ATTEMPTS {
        link.prepare_reconnect();
        match link.call("status", json!({})) {
            Ok(status) => {
                let actual = link.capabilities().identity.launch_id.as_deref();
                if actual != Some(expected_launch_id) {
                    return Err(LinkError::Emulator {
                        kind: "bad_state".into(),
                        message: format!(
                            "launch generation changed while recovering session cleanup \
                             (expected {expected_launch_id}, got {actual:?})"
                        ),
                    });
                }
                if method == "status" {
                    return Ok(status);
                }
            }
            Err(error @ (LinkError::NotConnected | LinkError::Timeout)) => {
                last = error;
                continue;
            }
            Err(error) => return Err(error),
        }
        match link.call(method, params.clone()) {
            Ok(value) => return Ok(value),
            Err(error @ (LinkError::NotConnected | LinkError::Timeout)) => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

/// Release a request-scoped input override and restore the frozen terminal state on every failure.
/// Calls continue after an individual cleanup error so a failed release does not suppress the
/// re-freeze attempt. A transport loss remains fail-loud: both cleanup failures are retained.
fn finish_transient_input<T>(
    link: &mut dyn EmulatorLink,
    port: u64,
    expected_launch_id: Option<&str>,
    outcome: Result<T, LinkError>,
) -> Result<T, LinkError> {
    let empty: [String; 0] = [];
    let release = call_session_cleanup(
        link,
        "set_input",
        json!({ "port": port, "buttons": empty }),
        expected_launch_id,
    )
    .map(|_| ());
    let result = finish_with_cleanup(outcome, release, combine_temporal_cleanup_error);
    finish_frozen(link, expected_launch_id, result)
}

fn finish_frozen<T>(
    link: &mut dyn EmulatorLink,
    expected_launch_id: Option<&str>,
    outcome: Result<T, LinkError>,
) -> Result<T, LinkError> {
    if outcome.is_ok() {
        return outcome;
    }
    let stabilize = call_session_cleanup(link, "pause", json!({}), expected_launch_id).map(|_| ());
    finish_with_cleanup(outcome, stabilize, combine_temporal_cleanup_error)
}

/// 프레임 단위 정밀 탭: freeze에서 정확히 press_frames만 입력을 주고 떼어, auto-repeat 없이
/// 결정론적 단일 입력(메뉴/타일 1칸)을 만든다. after_frames>0이면 떼고 그만큼 더 진행한다
/// (입력+관찰을 한 콜에 — frozen 유지). 호출 후 frozen 유지: 또 tap하거나 resume/run_frames.
pub fn tap(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    press_frames: u64,
    after_frames: u64,
) -> Result<ToolOutput, LinkError> {
    tap_axes(
        link,
        port,
        buttons,
        &Default::default(),
        press_frames,
        after_frames,
    )
}

fn tap_buttons(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    press_frames: u64,
    after_frames: u64,
) -> Result<ToolOutput, LinkError> {
    validate_sync_advance("tap press frame", press_frames)?;
    validate_sync_advance("tap trailing frame", after_frames)?;
    link.call("pause", json!({}))?; // 멱등
    let launch_id = link.capabilities().identity.launch_id.clone();
    if let Some((mut interrupted, phase)) =
        one_tap(link, port, buttons, press_frames, launch_id.as_deref())?
    {
        if let Some(object) = interrupted.as_object_mut() {
            object.insert("operation".into(), json!("tap"));
            object.insert("phase".into(), json!(phase));
            object.insert("tapped".into(), json!(buttons));
            object.insert("state".into(), json!("frozen"));
        }
        return Ok(ToolOutput::Json(interrupted));
    }
    if after_frames > 0 {
        let outcome = link.call("step", json!({ "frames": after_frames }));
        let trailing = finish_frozen(link, launch_id.as_deref(), outcome)?;
        if trailing.get("status").and_then(Value::as_str) == Some("interrupted") {
            let mut interrupted = trailing;
            if let Some(object) = interrupted.as_object_mut() {
                object.insert("operation".into(), json!("tap"));
                object.insert("phase".into(), json!("after_release"));
                object.insert("tapped".into(), json!(buttons));
                object.insert("state".into(), json!("frozen"));
            }
            return Ok(ToolOutput::Json(interrupted));
        }
    }
    Ok(ToolOutput::Json(json!({
        "tapped": buttons, "press_frames": press_frames, "after_frames": after_frames, "state": "frozen"
    })))
}

/// 버튼을 누른 채 frozen으로 프레임을 진행하며 watch 메모리를 보고, 값이 바뀌면 멈추고 뗀다
/// (실시간 타일 이동을 결정론적으로 — 좌표가 바뀔 때까지 방향 hold). max_frames까지 안 바뀌면
/// changed:false. 반환 {changed, frames, before, after}. 입력 효과 피드백·필드 이동에 쓴다.
#[allow(clippy::too_many_arguments)]
pub fn hold_until(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    memory_type: &str,
    address: u64,
    length: u64,
    max_frames: u64,
) -> Result<ToolOutput, LinkError> {
    validate_sync_advance("hold_until frame", max_frames)?;
    let read = |link: &mut dyn EmulatorLink| -> Result<String, LinkError> {
        let r = link.call(
            "read_memory",
            json!({ "memory_type": memory_type, "address": address, "length": length }),
        )?;
        Ok(r.get("hex")
            .and_then(|h| h.as_str())
            .unwrap_or("")
            .to_string())
    };
    link.call("pause", json!({}))?; // 멱등
    let launch_id = link.capabilities().identity.launch_id.clone();
    if let Err(primary) = link.call("set_input", json!({ "port": port, "buttons": buttons })) {
        return finish_transient_input(link, port, launch_id.as_deref(), Err(primary));
    }
    // 코어 루프를 돌리되 성패 무관하게 입력을 해제한다.
    let outcome: Result<(bool, u64, String, String), LinkError> = (|| {
        let before = read(link)?;
        let mut frames = 0u64;
        let mut after = before.clone();
        let mut changed = false;
        while frames < max_frames {
            link.call("step", json!({ "frames": 1 }))?;
            frames += 1;
            after = read(link)?;
            if after != before {
                changed = true;
                break;
            }
        }
        Ok((changed, frames, before, after))
    })();
    let (changed, frames, before, after) =
        finish_transient_input(link, port, launch_id.as_deref(), outcome)?;
    let release_edge = link.call("step", json!({ "frames": 1 })).map(|_| ());
    finish_frozen(link, launch_id.as_deref(), release_edge)?;
    Ok(ToolOutput::Json(json!({
        "changed": changed, "frames": frames, "before": before, "after": after, "state": "frozen"
    })))
}

pub fn load_state(link: &mut dyn EmulatorLink, path: &str) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("load_state", json!({ "path": path }))?,
    ))
}

/// n프레임을 실시간 진행한다. frozen이면 어댑터 run_frames 핸들러가 원자적으로 resume+advance하므로,
/// 여기서 별도 resume을 보내지 않는다 — 별도 resume은 명령 도착 전 free-run으로 watch_register/BP를 조기
/// 소진(one-shot)시키는 레이스다. 원자 resume이면 derail이 run_frames 구간에서 발화해 interrupted로 반환된다.
pub fn run_frames(link: &mut dyn EmulatorLink, n: u64) -> Result<ToolOutput, LinkError> {
    validate_sync_advance("run_frames", n)?;
    Ok(ToolOutput::Json(
        link.call("run_frames", json!({ "n": n }))?,
    ))
}

pub fn pause(link: &mut dyn EmulatorLink, cpu: Option<&str>) -> Result<ToolOutput, LinkError> {
    let mut params = json!({});
    with_cpu(link, &mut params, cpu)?;
    Ok(ToolOutput::Json(link.call("pause", params)?))
}

/// frozen에서 지정한 단위만큼 진행 후 재정지. 단위 확인과 실행은 한 번의 wire 호출 안에서 끝난다.
pub fn step(
    link: &mut dyn EmulatorLink,
    count: u64,
    unit: StepUnit,
    cpu: Option<&str>,
) -> Result<ToolOutput, LinkError> {
    validate_sync_advance("step", count)?;
    let required = unit.capability_method();
    if !link
        .capabilities()
        .methods
        .iter()
        .any(|method| method == required)
    {
        return Err(LinkError::Emulator {
            kind: "unsupported".into(),
            message: format!(
                "step unit `{}` is unavailable for this adapter; check status.contracts constraints",
                unit.as_str()
            ),
        });
    }
    let (method, mut params) = match unit {
        StepUnit::Frames => ("step", json!({ "frames": count })),
        StepUnit::Instructions => ("step_instructions", json!({ "count": count })),
        StepUnit::InputPolls => ("step_input_polls", json!({"count":count})),
    };
    with_cpu(link, &mut params, cpu)?;
    Ok(ToolOutput::Json(link.call(method, params)?))
}

pub fn resume(link: &mut dyn EmulatorLink, cpu: Option<&str>) -> Result<ToolOutput, LinkError> {
    let mut params = json!({});
    with_cpu(link, &mut params, cpu)?;
    Ok(ToolOutput::Json(link.call("resume", params)?))
}

fn call_reconnecting_operation(
    link: &mut dyn EmulatorLink,
    method: &str,
) -> Result<ToolOutput, LinkError> {
    let identity_before = link.capabilities().identity.clone();
    let mut result = link.call(method, json!({}))?;
    if result.get("reconnect").and_then(Value::as_bool) == Some(true) {
        link.prepare_reconnect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let status = loop {
            match link.call("status", json!({})) {
                Ok(status) if status.get("connected").and_then(Value::as_bool) == Some(true) => {
                    require_preserved_reconnect_identity(
                        method,
                        &identity_before,
                        &link.capabilities().identity,
                    )?;
                    break status;
                }
                Ok(_) | Err(LinkError::NotConnected | LinkError::Timeout | LinkError::Busy)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(LinkError::NoSuchEmulator { .. }) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Ok(_) => return Err(LinkError::Timeout),
                Err(error) => return Err(error),
            }
        };
        if let Some(object) = result.as_object_mut() {
            object.remove("reconnect");
            object.insert("reconnected".into(), Value::Bool(true));
            if let Some(state) = status.get("state") {
                object.insert("state".into(), state.clone());
            }
        }
    }
    Ok(ToolOutput::Json(result))
}

fn require_preserved_reconnect_identity(
    method: &str,
    before: &crate::live::link::EmulatorIdentity,
    after: &crate::live::link::EmulatorIdentity,
) -> Result<(), LinkError> {
    for (field, expected, actual) in [
        (
            "launch_id",
            before.launch_id.as_deref(),
            after.launch_id.as_deref(),
        ),
        (
            "content",
            before.content.as_deref(),
            after.content.as_deref(),
        ),
        ("system", before.system.as_deref(), after.system.as_deref()),
        (
            "adapter",
            before.adapter.as_deref(),
            after.adapter.as_deref(),
        ),
        ("build", before.build.as_deref(), after.build.as_deref()),
        (
            "session_token",
            before.session_token.as_deref(),
            after.session_token.as_deref(),
        ),
    ] {
        if expected.is_some() && expected != actual {
            return Err(LinkError::Emulator {
                kind: "identity_changed".into(),
                message: format!(
                    "{method} reconnected to a different {field}; refusing to report completion"
                ),
            });
        }
    }
    Ok(())
}

pub fn reset(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    call_reconnecting_operation(link, "reset")
}

pub fn power_cycle(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    call_reconnecting_operation(link, "power_cycle")
}

/// 메모리 접근 브레이크포인트(kind=exec/read/write). pc_min/pc_max를 주면 그 접근을 일으킨 명령의
/// 24비트 pc가 [pc_min,pc_max]일 때만 break한다(정상 push 등 노이즈 제거).
#[allow(clippy::too_many_arguments)]
pub fn set_breakpoint(
    link: &mut dyn EmulatorLink,
    kind: &str,
    memory_type: &str,
    start: u64,
    end: u64,
    pause_on_hit: bool,
    auto_savestate: bool,
    pc_min: Option<u64>,
    pc_max: Option<u64>,
    value: Option<u64>,
    value_mask: Option<u64>,
    value_len: Option<u64>,
    snapshot: &[String],
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({
        "kind": kind, "memory_type": memory_type, "start": start, "end": end,
        "pause_on_hit": pause_on_hit, "auto_savestate": auto_savestate,
    });
    // 히트 순간 atomic 캡처할 메모리 스펙(mt:addr:len 리스트). 어댑터가 record_hit에서 레지스터와 함께 잡는다.
    if !snapshot.is_empty() {
        params["snapshot"] = json!(snapshot);
    }
    if let Some(v) = pc_min {
        params["pc_min"] = json!(v);
    }
    if let Some(v) = pc_max {
        params["pc_max"] = json!(v);
    }
    // 값-조건(read/write BP): 접근 값이 (value & value_mask)와 같을 때만 break. value_len=비교 바이트(1~4).
    if let Some(v) = value {
        params["value"] = json!(v);
    }
    if let Some(v) = value_mask {
        params["value_mask"] = json!(v);
    }
    if let Some(v) = value_len {
        params["value_len"] = json!(v);
    }
    Ok(ToolOutput::Json(link.call("set_breakpoint", params)?))
}

/// 디스어셈블: address부터 count개 명령을 명령 단위로 디코드한다(SH-2=Mednafen, 65816=Mesen).
/// 코어가 가변 길이 명령 경계를 정확히 맞추므로 raw 바이트 수동 디코드가 불필요하다. BP 히트 PC
/// 주변을 바로 읽어 "어떤 명령이 이 접근을 일으켰나"를 즉시 본다. 반환 [{addr, text}].
pub fn disassemble(
    link: &mut dyn EmulatorLink,
    address: u64,
    count: u64,
    cpu: Option<&str>,
    mode: Option<&str>,
) -> Result<ToolOutput, LinkError> {
    let mut params = json!({ "address": address, "count": count });
    let selected_cpu = with_cpu(link, &mut params, cpu)?;
    let selected_mode = crate::debug_selection::resolve_disassembly_mode(
        &link.capabilities().identity.cpu_targets,
        selected_cpu.as_deref(),
        mode,
    )
    .map_err(bad_debug_parameter)?;
    if let (Some(mode), Some(params)) = (selected_mode, params.as_object_mut()) {
        params.insert("mode".into(), json!(mode));
    }
    Ok(ToolOutput::Json(link.call("disassemble", params)?))
}

/// 레지스터 범위 워치: register가 허용 범위 [min,max]를 벗어나는 명령에서 freeze한다(SP 폭주 등
/// derail을 그 명령에서 포착). register는 get_state의 cpu.* 이름(sp/pc/k/a/x/y/ps…). 매 명령 검사라
/// 느리니(실측 ~1fps) hunting 전용으로 쓰고 끝나면 clear한다.
/// watch_register 자동해제 예산 상한(명령 수). 이보다 크면 거부한다 — 무기한에 가까운 예산은 매 명령
/// getState 플러드로 emu 스레드를 오래 굶긴다. 기본(어댑터의 1M)의 여러 배까지 확장은 허용한다.
const MAX_WATCH_INSTRUCTIONS: u64 = 50_000_000;

pub fn watch_register(
    link: &mut dyn EmulatorLink,
    register: &str,
    min: u64,
    max: u64,
    pause_on_hit: bool,
    max_instructions: Option<u64>,
) -> Result<ToolOutput, LinkError> {
    let mut params =
        json!({ "register": register, "min": min, "max": max, "pause_on_hit": pause_on_hit });
    if let Some(budget) = max_instructions {
        if budget > MAX_WATCH_INSTRUCTIONS {
            return Err(LinkError::Emulator {
                kind: "bad_params".into(),
                message: format!(
                    "watch_register max_instructions {budget} exceeds the limit {MAX_WATCH_INSTRUCTIONS}"
                ),
            });
        }
        params["max_instructions"] = json!(budget);
    }
    Ok(ToolOutput::Json(link.call("watch_register", params)?))
}

pub fn clear_breakpoint(link: &mut dyn EmulatorLink, id: u64) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("clear_breakpoint", json!({ "id": id }))?,
    ))
}

pub fn list_breakpoints(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("list_breakpoints", json!({}))?))
}

pub fn clear_all_breakpoints(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("clear_all_breakpoints", json!({}))?,
    ))
}

pub fn poll_events(link: &mut dyn EmulatorLink) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(link.call("poll_events", json!({}))?))
}

/// 실행추적 on/off. 켜면 매 명령 콜백이 콜스택·트레이스를 유지한다(느림 — 크래시 추적 hunting 전용).
pub fn set_trace(link: &mut dyn EmulatorLink, enabled: bool) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("set_trace", json!({ "enabled": enabled }))?,
    ))
}

/// 최근 count개 실행 명령을 시간순으로(트레이스 링버퍼). set_trace(true)가 선행돼야 함.
pub fn get_trace(link: &mut dyn EmulatorLink, count: u64) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("get_trace", json!({ "count": count }))?,
    ))
}

/// 현재 콜스택을 backend가 광고한 authority로 반환한다. 일부 backend는 실행 추적에서 호출 체인을
/// 재구성하고, 다른 backend는 정지 시점의 프레임 포인터나 네이티브 unwinder를 사용한다.
pub fn call_stack(link: &mut dyn EmulatorLink, cpu: Option<&str>) -> Result<ToolOutput, LinkError> {
    let mut params = json!({});
    with_cpu(link, &mut params, cpu)?;
    Ok(ToolOutput::Json(link.call("call_stack", params)?))
}

/// break_on_reset: 게임이 리셋 핸들러($00:FFFC 벡터)를 실행하면 freeze(워치독 리셋·하드 크래시→리셋
/// 자동 감지). enabled로 on/off. 단일 주소 exec BP라 빠르다(per-instruction 아님).
pub fn break_on_reset(link: &mut dyn EmulatorLink, enabled: bool) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("break_on_reset", json!({ "enabled": enabled }))?,
    ))
}

#[cfg(test)]
#[path = "tools_unit_tests.rs"]
mod tests;
