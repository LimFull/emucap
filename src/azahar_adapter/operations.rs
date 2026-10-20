use super::observation::AdvanceStop;
use super::*;

const REGISTER_NAMES: [&str; REGISTER_COUNT] = [
    "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12", "sp", "lr",
    "pc", "cpsr",
];

impl AzaharHost {
    fn execution_limits() -> Value {
        json!({
            "max_sync_advance_count": MAX_SYNC_FRAMES,
            "max_sync_operation_ms": crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64,
            "frame": {"max_count": MAX_SYNC_FRAMES}
        })
    }

    fn capability_notes() -> Value {
        json!({
            "implemented_methods": METHODS,
            "step_units": ["frames"],
            "frame_boundary": "one retro_run, which ends at the next presented VBlank",
            "memory": "virtual is the application's 32-bit address space; unmapped pages reject the access",
            "registers": "r0-r15 and CPSR of the application core (core 0) at the frame boundary",
            "touch": {"screen": "bottom", "width": TOUCH_WIDTH, "height": TOUCH_HEIGHT},
            "screenshot_layout": "top screen above the centered bottom screen",
            "breakpoints": false,
            "disassembly": false,
            "audio_output": false
        })
    }

    pub(super) fn hello(&self) -> AzaharResult<Value> {
        let mut value = json!({
            "protocol_version": PROTOCOL_VERSION,
            "system": SYSTEM,
            "adapter": ADAPTER,
            "backend": ADAPTER,
            "debugger": true,
            "methods": METHODS,
            "memory_types": ["virtual"],
            "state_groups": ["cpu"],
            "cpu_targets": [{
                "id":"main", "aliases":["arm11", "core0"], "default":true, "disassembly_modes":[]
            }],
            "region_sizes": {"virtual": VIRTUAL_SPACE_SIZE},
            "breakpoint_kinds": [],
            "input_buttons": input_buttons(),
            "execution_limits": Self::execution_limits(),
            "contracts": crate::contracts::advertisement_value(ACTIVE_EXCEPTIONS),
            "content": self.content_path.display().to_string(),
            "build": self.frontend_build,
            "host_build": {
                "upstream_version": self.upstream_version,
                "patchset_sha256": self.patchset_sha256,
                "core_sha256": self.core_sha256,
                "host_api": 1
            },
            "capability_notes": Self::capability_notes(),
            "runtime_home": self.runtime_home.display().to_string(),
            "memory_batch_capability": Self::memory_batch_capability(),
            "execution_speed_capability": Self::execution_speed_capability()
        });
        let object = value.as_object_mut().expect("Azahar hello object");
        if let Some(name) = &self.name {
            object.insert("name".into(), json!(name));
        }
        if let Some(launch_id) = &self.launch_id {
            object.insert("launch_id".into(), json!(launch_id));
        }
        Ok(value)
    }

    pub(super) fn status(&self) -> AzaharResult<Value> {
        Ok(json!({
            "connected": true,
            "system": SYSTEM,
            "adapter": ADAPTER,
            "backend": ADAPTER,
            "debugger": true,
            "methods": METHODS,
            "memory_types": ["virtual"],
            "region_sizes": {"virtual": VIRTUAL_SPACE_SIZE},
            "input_buttons": {"buttons": input_buttons(), "available": input_buttons()},
            "input_override": {
                "engaged": !self.held_buttons.is_empty() || self.held_touch.is_some(),
                "owner": if self.held_buttons.is_empty() && self.held_touch.is_none() {"frontend"} else {"mcp"},
                "buttons": self.held_buttons.iter().collect::<Vec<_>>(),
                "touch": self.held_touch.map(|(x, y)| json!({"x": x, "y": y}))
            },
            "contracts": crate::contracts::advertisement_value(ACTIVE_EXCEPTIONS),
            "state": if self.frozen {"frozen"} else {"running"},
            "frame": self.frame,
            "execution_speed": self.pacing_policy_value(),
            "video_fresh": self.video_fresh,
            "execution_limits": Self::execution_limits(),
            "capability_notes": Self::capability_notes()
        }))
    }

    pub(super) fn get_rom_info(&self) -> AzaharResult<Value> {
        let mut program_id = 0_u64;
        let title_id = (unsafe { (self.api.program_id)(&mut program_id) } != 0)
            .then(|| format!("{program_id:016x}"));
        Ok(json!({
            "system": SYSTEM,
            "content": self.content_path.display().to_string(),
            "name": self.content_path.file_name().and_then(|v| v.to_str()).unwrap_or(""),
            "rom_size": self.content_size,
            "rom_sha1": self.content_sha1,
            "sha1": self.content_sha1,
            "title_id": title_id,
            "content_identity_scope": "complete_file",
            "backend": ADAPTER,
            "core_sha256": self.core_sha256
        }))
    }

    pub(super) fn read_virtual(&self, address: u32, length: usize) -> AzaharResult<Vec<u8>> {
        let mut bytes = vec![0_u8; length];
        if unsafe { (self.api.read_memory)(address, bytes.as_mut_ptr(), length) } == 0 {
            return Err(AzaharError::BadParams(format!(
                "{address:#x}+{length:#x} is not mapped in the application process"
            )));
        }
        Ok(bytes)
    }

    pub(super) fn read_memory(&mut self, params: &Value) -> AzaharResult<Value> {
        let length = required_num(params, "length")?;
        let address = virtual_address(params, length)?;
        let bytes = self.read_virtual(address, length as usize)?;
        Ok(
            json!({"address": address, "length": length, "hex": hex::encode(bytes), "frame": self.frame}),
        )
    }

    pub(super) fn write_memory(&mut self, params: &Value) -> AzaharResult<Value> {
        let raw = params
            .get("hex")
            .or_else(|| params.get("data"))
            .and_then(Value::as_str)
            .ok_or_else(|| AzaharError::BadParams("missing required param: hex".into()))?;
        let data = hex::decode(raw)
            .map_err(|_| AzaharError::BadParams("hex must contain complete bytes".into()))?;
        let address = virtual_address(params, data.len() as u64)?;
        if unsafe { (self.api.write_memory)(address, data.as_ptr(), data.len()) } == 0 {
            return Err(AzaharError::BadParams(format!(
                "{address:#x}+{:#x} is not mapped in the application process",
                data.len()
            )));
        }
        self.boundary_seq += 1;
        Ok(json!({"written": data.len(), "address": address}))
    }

    pub(super) fn get_state(&self) -> AzaharResult<Value> {
        let mut registers = [0_u32; REGISTER_COUNT];
        if unsafe { (self.api.get_registers)(registers.as_mut_ptr(), REGISTER_COUNT) } == 0 {
            return Err(AzaharError::BadState(
                "the application core is not available".into(),
            ));
        }
        let cpu = REGISTER_NAMES
            .iter()
            .zip(registers)
            .map(|(name, value)| ((*name).to_string(), json!(value)))
            .collect::<serde_json::Map<_, _>>();
        Ok(json!({"cpu": cpu, "frame": self.frame}))
    }

    pub(super) fn pause(&mut self) -> AzaharResult<Value> {
        self.frozen = true;
        Ok(json!({"status":"completed", "state":"frozen", "frame":self.frame}))
    }

    pub(super) fn resume(&mut self) -> AzaharResult<Value> {
        // Frozen time never accumulates pacing debt.
        self.pacer.reanchor();
        self.frozen = false;
        Ok(json!({"status":"completed", "state":"running", "frame":self.frame}))
    }

    fn advance_value(
        &self,
        before: u64,
        requested: u64,
        completed: u64,
        stop: Option<AdvanceStop>,
    ) -> Value {
        json!({
            "status": if stop.is_none() {"completed"} else {"interrupted"},
            "reason": AdvanceStop::reason(stop),
            "unit": "frames", "count": completed, "requested": requested, "completed": completed,
            "frame_before": before, "frame": self.frame,
            "state": if self.frozen {"frozen"} else {"running"}
        })
    }

    pub(super) fn step(&mut self, params: &Value) -> AzaharResult<Value> {
        require_frame_unit(params)?;
        let count = frame_count(params)?;
        let before = self.frame;
        self.frozen = true;
        let (completed, stop) = self.run_paced_frames(count)?;
        Ok(self.advance_value(before, count, completed, stop))
    }

    pub(super) fn run_frames(&mut self, params: &Value) -> AzaharResult<Value> {
        require_frame_unit(params)?;
        let count = frame_count(params)?;
        let was_frozen = self.frozen;
        let before = self.frame;
        let (completed, stop) = self.run_paced_frames(count)?;
        self.frozen = was_frozen || stop.is_some();
        Ok(self.advance_value(before, count, completed, stop))
    }

    fn apply_held_controls(&self) {
        set_controls(&self.held_buttons, self.held_touch);
    }

    pub(super) fn set_input(&mut self, params: &Value) -> AzaharResult<Value> {
        require_port_zero(params)?;
        self.held_buttons = normalize_buttons(params.get("buttons"))?;
        self.apply_held_controls();
        Ok(json!({
            "status":"completed",
            "buttons":self.held_buttons.iter().collect::<Vec<_>>(),
            "override_engaged":!self.held_buttons.is_empty(),
            "ownership":if self.held_buttons.is_empty() {"frontend"} else {"mcp"}
        }))
    }

    fn pulse_frames(params: &Value, operation: &str) -> AzaharResult<u64> {
        let frames = optional_num(params, "frames")?.unwrap_or(1);
        if (1..=MAX_INPUT_PULSE_FRAMES).contains(&frames) {
            Ok(frames)
        } else {
            Err(AzaharError::BadParams(format!(
                "{operation} frames must be in 1..={MAX_INPUT_PULSE_FRAMES}, got {frames}"
            )))
        }
    }

    /// Runs `frames` with transient controls applied, then restores the held controls.
    fn run_pulse(
        &mut self,
        frames: u64,
        buttons: &BTreeSet<String>,
        touch: Option<(u64, u64)>,
    ) -> AzaharResult<(u64, Option<AdvanceStop>)> {
        let was_frozen = self.frozen;
        set_controls(buttons, touch);
        let result = self.run_paced_frames(frames);
        self.apply_held_controls();
        let (completed, stop) = result?;
        self.frozen = was_frozen || stop.is_some();
        Ok((completed, stop))
    }

    pub(super) fn press_buttons(&mut self, params: &Value) -> AzaharResult<Value> {
        require_port_zero(params)?;
        let pulse = normalize_buttons(params.get("buttons"))?;
        if pulse.is_empty() {
            return Err(AzaharError::BadParams(
                "press_buttons requires at least one button".into(),
            ));
        }
        let frames = Self::pulse_frames(params, "press_buttons")?;
        let active = self.held_buttons.union(&pulse).cloned().collect();
        let (completed, stop) = self.run_pulse(frames, &active, self.held_touch)?;
        Ok(json!({
            "status":if stop.is_none() {"completed"} else {"interrupted"},
            "reason":AdvanceStop::reason(stop),
            "buttons":pulse.iter().collect::<Vec<_>>(), "frames":frames, "completed":completed,
            "persistent_buttons":self.held_buttons.iter().collect::<Vec<_>>(),
            "state":if self.frozen {"frozen"} else {"running"}, "frame":self.frame
        }))
    }

    /// Touches the 320x240 bottom screen. `release` lifts, `frames` taps and lifts, and omitting
    /// both holds the touch until the next touch command.
    pub(super) fn touch(&mut self, params: &Value) -> AzaharResult<Value> {
        require_port_zero(params)?;
        if params
            .get("release")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.held_touch = None;
            self.apply_held_controls();
            return Ok(json!({"status":"completed", "released": true, "override_engaged": false}));
        }
        let x = required_num(params, "x")?;
        let y = required_num(params, "y")?;
        if x >= TOUCH_WIDTH || y >= TOUCH_HEIGHT {
            return Err(AzaharError::BadParams(format!(
                "touch out of range: x 0-{}, y 0-{} (got x={x}, y={y})",
                TOUCH_WIDTH - 1,
                TOUCH_HEIGHT - 1
            )));
        }
        if params.get("frames").is_none() {
            self.held_touch = Some((x, y));
            self.apply_held_controls();
            return Ok(json!({"status":"completed", "x": x, "y": y, "override_engaged": true}));
        }
        let frames = Self::pulse_frames(params, "touch")?;
        let buttons = self.held_buttons.clone();
        let (completed, stop) = self.run_pulse(frames, &buttons, Some((x, y)))?;
        Ok(json!({
            "status":if stop.is_none() {"completed"} else {"interrupted"},
            "reason":AdvanceStop::reason(stop),
            "x": x, "y": y, "frames": frames, "frames_elapsed": completed,
            "override_engaged": self.held_touch.is_some(),
            "state":if self.frozen {"frozen"} else {"running"}, "frame":self.frame
        }))
    }

    pub(super) fn screenshot(&self) -> AzaharResult<Value> {
        self.require_frozen("screenshot")?;
        if !self.video_fresh {
            return Err(AzaharError::BadState(
                "screenshot is unavailable until one frame completes after launch, load_state or reset".into(),
            ));
        }
        let (png, width, height) = captured_png()?;
        Ok(json!({
            "png_base64":base64::engine::general_purpose::STANDARD.encode(&png),
            "sha256":hex::encode(Sha256::digest(&png)), "byte_len":png.len(),
            "width":width, "height":height, "frame":self.frame, "frame_stable":true,
            "state":"frozen", "freshness":"current",
            "capture_boundary":"last_completed_retro_run_video_callback"
        }))
    }

    pub(super) fn save_state(&mut self, params: &Value) -> AzaharResult<Value> {
        self.require_frozen("save_state")?;
        let path = absolute_path_param(params, "path")?;
        let size = unsafe { (self.api.serialize_size)() } as u64;
        if size == 0 || size > MAX_STATE_BYTES {
            return Err(AzaharError::Core(format!(
                "Azahar serialization size {size} is outside 1..={MAX_STATE_BYTES}"
            )));
        }
        let mut data = vec![0_u8; size as usize];
        if !unsafe { (self.api.serialize)(data.as_mut_ptr().cast(), data.len()) } {
            return Err(AzaharError::Core("retro_serialize failed".into()));
        }
        crate::path_safety::atomic_write_file(&path, &data)?;
        let state_sha256 = hex::encode(Sha256::digest(&data));
        let sidecar = state_sidecar(&path);
        crate::path_safety::atomic_write_file(
            &sidecar,
            &serde_json::to_vec_pretty(&self.state_identity(state_sha256.clone()))?,
        )?;
        Ok(json!({
            "status":"completed", "path":path.display().to_string(),
            "sidecar":sidecar.display().to_string(), "format":"azahar-libretro-state",
            "bytes":data.len(), "sha256":state_sha256, "state":"frozen", "frame":self.frame
        }))
    }

    pub(super) fn load_state(&mut self, params: &Value) -> AzaharResult<Value> {
        self.require_frozen("load_state")?;
        let path = absolute_path_param(params, "path")?;
        let read = |path: &Path, limit: u64, what: &str| {
            crate::path_safety::read_bounded_regular_file_no_follow(path, limit).map_err(|error| {
                AzaharError::BadParams(format!("invalid 3DS state {what}: {error}"))
            })
        };
        let data = read(&path, MAX_STATE_BYTES, "file")?;
        let identity: StateIdentity = serde_json::from_slice(&read(
            &state_sidecar(&path),
            MAX_STATE_SIDECAR_BYTES,
            "sidecar",
        )?)
        .map_err(|error| AzaharError::BadParams(format!("invalid 3DS state sidecar: {error}")))?;
        if identity != self.state_identity(hex::encode(Sha256::digest(&data))) {
            return Err(AzaharError::BadParams("save state identity does not match this exact title, core, patch set, frontend build, OS, and architecture".into()));
        }
        // A native failure may occur after partial mutation, so the prior capture is stale either way.
        self.video_fresh = false;
        self.boundary_seq += 1;
        if !unsafe { (self.api.unserialize)(data.as_ptr().cast(), data.len()) } {
            return Err(AzaharError::Core(
                "retro_unserialize failed; machine remains frozen but restored state is unverified"
                    .into(),
            ));
        }
        self.frozen = true;
        Ok(json!({
            "status":"completed", "path":path.display().to_string(), "format":"azahar-libretro-state",
            "state":"frozen", "frame":self.frame,
            "screenshot_freshness":"unverified_until_frozen_frame_step"
        }))
    }

    pub(super) fn reset(&mut self) -> AzaharResult<Value> {
        self.boundary_seq += 1;
        unsafe { (self.api.reset)() };
        self.frozen = true;
        self.video_fresh = false;
        Ok(
            json!({"status":"completed", "state":"frozen", "frame":self.frame, "guest_frames_advanced":0}),
        )
    }

    pub(super) fn run_one_frame(&mut self) -> AzaharResult<()> {
        let before = callback_count()?;
        unsafe { (self.api.run)() };
        let after = callback_count()?;
        if after != before + 1 {
            return Err(AzaharError::Core(format!(
                "retro_run did not produce exactly one video callback: before={before}, after={after}"
            )));
        }
        self.frame = self
            .frame
            .checked_add(1)
            .ok_or_else(|| AzaharError::Core("frontend frame counter overflow".into()))?;
        self.video_fresh = true;
        Ok(())
    }

    pub(super) fn require_frozen(&self, operation: &str) -> AzaharResult<()> {
        if self.frozen {
            Ok(())
        } else {
            Err(AzaharError::BadState(format!(
                "{operation} requires a frozen 3DS; call pause first"
            )))
        }
    }

    fn state_identity(&self, state_sha256: String) -> StateIdentity {
        StateIdentity {
            format: "azahar-libretro-state".into(),
            system: SYSTEM.into(),
            target_os: std::env::consts::OS.into(),
            target_arch: std::env::consts::ARCH.into(),
            media_sha1: self.content_sha1.clone(),
            core_sha256: self.core_sha256.clone(),
            upstream_version: self.upstream_version.clone(),
            patchset_sha256: self.patchset_sha256.clone(),
            frontend_build: self.frontend_build.clone(),
            state_sha256,
        }
    }
}
