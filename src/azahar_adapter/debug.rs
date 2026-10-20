//! Interpreter breakpoints, instruction steps, disassembly and call stacks.
use super::*;

const MAX_BREAKPOINTS: usize = 128;
const MAX_PENDING_EVENTS: usize = 256;
const MAX_SNAPSHOTS: usize = 16;
const MAX_SNAPSHOT_BYTES: u64 = 0x4000;
const MAX_INSTRUCTION_STEPS: u64 = 1000;
const MAX_DISASSEMBLY_ROWS: u64 = 64;
/// Frames a single instruction step may wait for the application to be scheduled.
const MAX_STEP_WAIT_FRAMES: u32 = 120;
const KIND_NAMES: [&str; 4] = ["exec", "read", "write", "access"];
pub(super) const REGISTER_NAMES: [&str; REGISTER_COUNT] = [
    "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12", "sp", "lr",
    "pc", "cpsr",
];

#[derive(Clone, Debug)]
pub(super) struct DebugBreakpoint {
    kind: &'static str,
    start: u32,
    end: u32,
    pause_on_hit: bool,
    snapshots: Vec<(u32, u32)>,
}

pub(super) fn breakpoint_kinds() -> Value {
    KIND_NAMES
        .iter()
        .map(|kind| {
            json!({
                "kind": kind, "range_unit": "address", "range_mode": "inclusive",
                "memory_type_used": true, "pause_on_hit": [true, false],
                "snapshot": "pause_on_hit",
                "snapshot_timing": if *kind == "exec" {"pre_instruction"} else {"post_access"},
                "value": if *kind == "exec" {"unavailable"} else {"authoritative"}
            })
        })
        .collect()
}

pub(super) fn registers_value(registers: &[u32; REGISTER_COUNT]) -> Value {
    REGISTER_NAMES
        .iter()
        .zip(registers)
        .map(|(name, value)| ((*name).to_string(), json!(value)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

pub(super) fn parse_snapshots(raw: Option<&Value>) -> AzaharResult<Vec<(u32, u32)>> {
    let Some(raw) = raw.filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let items = raw
        .as_array()
        .ok_or_else(|| AzaharError::BadParams("snapshot must be a list".into()))?;
    if items.len() > MAX_SNAPSHOTS {
        return Err(AzaharError::BadParams(format!(
            "snapshot accepts at most {MAX_SNAPSHOTS} ranges"
        )));
    }
    let mut total = 0_u64;
    items
        .iter()
        .map(|item| {
            let text = item.as_str().unwrap_or("");
            let parts = text.split(':').collect::<Vec<_>>();
            let [memory_type, address, length] = parts.as_slice() else {
                return Err(AzaharError::BadParams(format!(
                    "snapshot entries must be virtual:address:length, got {item}"
                )));
            };
            let length = crate::numparse::parse_num_str(length).unwrap_or(0);
            total = total.saturating_add(length);
            if total > MAX_SNAPSHOT_BYTES {
                return Err(AzaharError::BadParams(format!(
                    "snapshot byte total exceeds {MAX_SNAPSHOT_BYTES:#x}"
                )));
            }
            let params = json!({"memory_type": memory_type, "address": address});
            Ok((virtual_address(&params, length)?, length as u32))
        })
        .collect()
}

impl AzaharHost {
    fn require_debugger(&self) -> AzaharResult<()> {
        if unsafe { (self.api.debug_available)() } == 0 {
            return Err(AzaharError::Unsupported(
                "breakpoints and instruction steps need the interpreter CPU".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn set_breakpoint(&mut self, params: &Value) -> AzaharResult<Value> {
        self.require_debugger()?;
        for key in [
            "condition",
            "value",
            "value_mask",
            "value_len",
            "pc_min",
            "pc_max",
        ] {
            if params.get(key).is_some_and(|value| !value.is_null()) {
                return Err(AzaharError::BadParams(format!(
                    "3DS breakpoint option {key} is not supported"
                )));
            }
        }
        if params.get("auto_savestate").and_then(Value::as_bool) == Some(true) {
            return Err(AzaharError::BadParams(
                "3DS breakpoints do not support auto_savestate; save after the hit".into(),
            ));
        }
        let kind_name = params.get("kind").and_then(Value::as_str).unwrap_or("exec");
        let kind = KIND_NAMES
            .iter()
            .position(|known| *known == kind_name)
            .ok_or_else(|| {
                AzaharError::BadParams(format!(
                    "unsupported 3DS breakpoint kind {kind_name}; supported: exec, read, write, access"
                ))
            })?;
        let start = required_num(params, "start")?;
        let end = optional_num(params, "end")?.unwrap_or(start);
        if end < start {
            return Err(AzaharError::BadParams(
                "breakpoint end must be greater than or equal to start".into(),
            ));
        }
        let memory_type = match params.get("memory_type").and_then(Value::as_str) {
            None | Some("cpu") => "virtual",
            Some(other) => other,
        };
        let span = end - start + 1;
        let range = json!({"memory_type": memory_type, "address": start});
        // The range bound alone; a breakpoint may watch memory that is not mapped yet.
        virtual_address(&range, span.min(MAX_MEMORY_BYTES))?;
        if end >= VIRTUAL_SPACE_SIZE {
            return Err(AzaharError::BadParams(
                "breakpoint range exceeds the 32-bit address space".into(),
            ));
        }
        let pause_on_hit = params
            .get("pause_on_hit")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let snapshots = parse_snapshots(params.get("snapshot"))?;
        if !pause_on_hit && !snapshots.is_empty() {
            return Err(AzaharError::BadParams(
                "3DS snapshots require pause_on_hit=true; they describe the frozen boundary".into(),
            ));
        }
        if self.breakpoints.len() >= MAX_BREAKPOINTS {
            return Err(AzaharError::BadParams(format!(
                "3DS supports at most {MAX_BREAKPOINTS} breakpoints"
            )));
        }
        let id = self.next_breakpoint_id;
        let native = NativePoint {
            id,
            kind: kind as u32,
            start: start as u32,
            end: end as u32,
            pause_on_hit: u32::from(pause_on_hit),
        };
        if unsafe { (self.api.set_point)(&native) } == 0 {
            return Err(AzaharError::Core(format!(
                "Azahar rejected breakpoint {id}"
            )));
        }
        self.next_breakpoint_id += 1;
        self.breakpoints.insert(
            id,
            DebugBreakpoint {
                kind: KIND_NAMES[kind],
                start: start as u32,
                end: end as u32,
                pause_on_hit,
                snapshots,
            },
        );
        Ok(json!({"id": id, "set": true, "arm_state": "armed"}))
    }

    pub(super) fn clear_breakpoint(&mut self, params: &Value) -> AzaharResult<Value> {
        let id = required_num(params, "id")?;
        if !self.breakpoints.contains_key(&id) {
            return Err(AzaharError::BadParams(format!(
                "unknown breakpoint id: {id}"
            )));
        }
        self.drain_native_events()?;
        if unsafe { (self.api.clear_point)(id) } == 0 {
            return Err(AzaharError::Core(format!(
                "Azahar failed to clear breakpoint {id}"
            )));
        }
        self.breakpoints.remove(&id);
        Ok(json!({"cleared": id}))
    }

    pub(super) fn list_breakpoints(&self) -> AzaharResult<Value> {
        let rows = self
            .breakpoints
            .iter()
            .map(|(id, bp)| {
                json!({"id": id, "kind": bp.kind, "memory_type": "virtual",
                    "start": bp.start, "end": bp.end, "pause_on_hit": bp.pause_on_hit,
                    "arm_state": "armed",
                    "snapshot": bp.snapshots.iter()
                        .map(|(address, length)| format!("virtual:{address:#x}:{length:#x}"))
                        .collect::<Vec<_>>()})
            })
            .collect::<Vec<_>>();
        Ok(json!({"breakpoints": rows}))
    }

    pub(super) fn clear_all_breakpoints(&mut self) -> AzaharResult<Value> {
        self.drain_native_events()?;
        unsafe { (self.api.clear_points)() };
        let cleared = std::mem::take(&mut self.breakpoints)
            .into_keys()
            .collect::<Vec<_>>();
        Ok(json!({"cleared": cleared}))
    }

    pub(super) fn poll_events(&mut self, params: &Value) -> AzaharResult<Value> {
        let filter = optional_num(params, "breakpoint_id")?;
        self.drain_native_events()?;
        let (events, kept): (Vec<_>, Vec<_>) = self
            .pending_events
            .drain(..)
            .partition(|event| filter.is_none_or(|id| event["breakpoint_id"] == json!(id)));
        self.pending_events = kept.into();
        let dropped =
            std::mem::take(&mut self.dropped_events) + unsafe { (self.api.take_dropped)() };
        Ok(json!({"events": events, "dropped": dropped, "pending": self.pending_events.len()}))
    }

    /// Moves native hit records into the bounded host queue. A pausing hit's snapshots describe
    /// the frozen boundary, so they are read here, before the guest runs again.
    pub(super) fn drain_native_events(&mut self) -> AzaharResult<()> {
        loop {
            let mut native = NativeEvent::default();
            if unsafe { (self.api.poll_event)(&mut native) } == 0 {
                return Ok(());
            }
            let kind = KIND_NAMES
                .get(native.kind as usize)
                .copied()
                .unwrap_or("unknown");
            let mut event = json!({
                "type": "breakpoint_hit",
                "id": native.breakpoint_id,
                "breakpoint_id": native.breakpoint_id,
                "sequence": native.sequence,
                "kind": kind,
                "address": native.address,
                "size": native.size,
                "pc": native.registers[15],
                "paused": native.paused != 0,
                "regs": registers_value(&native.registers),
                "frame": self.frame,
            });
            if kind != "exec" && native.size <= 4 {
                event["value"] = json!(native.value);
            }
            let snapshots = self
                .breakpoints
                .get(&native.breakpoint_id)
                .map(|bp| bp.snapshots.clone())
                .unwrap_or_default();
            if native.paused != 0 && !snapshots.is_empty() {
                let captured = snapshots
                    .iter()
                    .map(|(address, length)| {
                        let read = self.read_virtual(*address, *length as usize);
                        json!({"memory_type": "virtual", "address": address, "length": length,
                            "hex": read.as_ref().ok().map(hex::encode),
                            "error": read.err().map(|error| error.to_string())})
                    })
                    .collect::<Vec<_>>();
                event["snapshot"] = json!(captured);
            }
            if self.pending_events.len() == MAX_PENDING_EVENTS {
                self.pending_events.pop_front();
                self.dropped_events += 1;
            }
            self.pending_events.push_back(event);
        }
    }

    pub(super) fn step_instructions(&mut self, params: &Value) -> AzaharResult<Value> {
        self.require_debugger()?;
        let count = optional_num(params, "count")?
            .or(optional_num(params, "n")?)
            .unwrap_or(1);
        if !(1..=MAX_INSTRUCTION_STEPS).contains(&count) {
            return Err(AzaharError::BadParams(format!(
                "instruction count must be in 1..={MAX_INSTRUCTION_STEPS}, got {count}"
            )));
        }
        self.frozen = true;
        let frame_before = self.frame;
        let mut completed = 0;
        let mut reason = Value::Null;
        'steps: while completed < count {
            if self.request_cancellation.is_cancelled() {
                reason = json!("cancelled");
                break;
            }
            unsafe { (self.api.request_step)(1) };
            let mut waited = 0;
            let stop = loop {
                let (_, stop) = self.run_native()?;
                if let Some(stop) = stop {
                    break stop;
                }
                waited += 1;
                if waited == MAX_STEP_WAIT_FRAMES {
                    unsafe { (self.api.request_step)(0) };
                    reason = json!("application_not_scheduled");
                    break 'steps;
                }
            };
            if stop == STOP_BREAKPOINT {
                reason = json!("breakpoint");
                break;
            }
            completed += 1;
        }
        let registers = self.registers()?;
        Ok(json!({
            "status": if completed == count {"completed"} else {"interrupted"},
            "reason": reason, "unit": "instructions", "count": completed, "requested": count,
            "pc": registers[15], "frame_before": frame_before, "frame": self.frame,
            "state": "frozen"
        }))
    }

    pub(super) fn registers(&self) -> AzaharResult<[u32; REGISTER_COUNT]> {
        let mut registers = [0_u32; REGISTER_COUNT];
        if unsafe { (self.api.get_registers)(registers.as_mut_ptr(), REGISTER_COUNT) } == 0 {
            return Err(AzaharError::BadState(
                "the application core is not available".into(),
            ));
        }
        Ok(registers)
    }

    pub(super) fn disassemble(&mut self, params: &Value) -> AzaharResult<Value> {
        let address = required_num(params, "address")?;
        let count = optional_num(params, "count")?.unwrap_or(16);
        if !(1..=MAX_DISASSEMBLY_ROWS).contains(&count) || address >= VIRTUAL_SPACE_SIZE {
            return Err(AzaharError::BadParams(format!(
                "disassemble needs an address below 2^32 and count in 1..={MAX_DISASSEMBLY_ROWS}"
            )));
        }
        let thumb = match params.get("mode").and_then(Value::as_str) {
            None | Some("auto") => self.registers()?[16] & (1 << 5) != 0,
            Some("arm") => false,
            Some("thumb") => true,
            Some(other) => {
                return Err(AzaharError::BadParams(format!(
                    "unsupported 3DS disassembly mode {other}; supported: auto, arm, thumb"
                )))
            }
        };
        let mut rows = Vec::new();
        let mut cursor = address as u32;
        for _ in 0..count {
            let mut text = [0 as c_char; 256];
            let mut length = 0_u32;
            if unsafe {
                (self.api.disassemble)(
                    cursor,
                    i32::from(thumb),
                    text.as_mut_ptr(),
                    text.len(),
                    &mut length,
                )
            } == 0
            {
                if rows.is_empty() {
                    return Err(AzaharError::BadParams(format!(
                        "{cursor:#x} is not a mapped, aligned {} instruction",
                        if thumb { "Thumb" } else { "ARM" }
                    )));
                }
                break;
            }
            let bytes = self.read_virtual(cursor, length as usize)?;
            let text = unsafe { CStr::from_ptr(text.as_ptr()) }.to_string_lossy();
            rows.push(json!({"addr": cursor, "text": text, "bytes": hex::encode(bytes), "length": length}));
            cursor = cursor.wrapping_add(length);
        }
        Ok(json!({"instructions": rows, "mode": if thumb {"thumb"} else {"arm"}}))
    }

    /// Without frame pointers in 3DS code, only PC and the link register are reliable.
    pub(super) fn call_stack(&mut self) -> AzaharResult<Value> {
        let registers = self.registers()?;
        Ok(json!({
            "frames": [
                {"kind": "pc", "pc": registers[15], "sp": registers[13]},
                {"kind": "return", "pc": registers[14] & !1}
            ],
            "depth": 2, "cpu": "arm11", "order": "innermost_to_outermost",
            "method": "link_register", "authority": "best_effort",
            "complete_since_reset": false, "truncated": true, "max_depth": 2,
            "note": "the return frame is the link register, which is stale once the function has made a call"
        }))
    }
}
