//! Raw controller encoding and request-owned restoration.
use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ControllerState {
    pub buttons: BTreeSet<String>,
    pub axes: BTreeMap<String, i64>,
    pub engaged: bool,
}
impl ControllerState {
    pub fn decode(params: &Value) -> N64Result<Self> {
        control::require_port_zero(params)?;
        let buttons = control::normalize_buttons(params.get("buttons"))?;
        let axes = match params.get("axes") {
            None => BTreeMap::new(),
            Some(value) => serde_json::from_value::<BTreeMap<String, i64>>(value.clone())
                .map_err(|_| N64Error::BadParams("axes must be an integer map".into()))?,
        };
        for (name, value) in &axes {
            if !matches!(name.as_str(), "stick_x" | "stick_y") || !(-128..=127).contains(value) {
                return Err(N64Error::BadParams(format!(
                    "invalid N64 axis {name}: {value}"
                )));
            }
        }
        let state = Self {
            engaged: !buttons.is_empty() || !axes.is_empty(),
            buttons,
            axes,
        };
        state.validate()?;
        Ok(state)
    }
    fn validate(&self) -> N64Result<()> {
        if !self.axes.is_empty()
            && self
                .buttons
                .iter()
                .any(|b| matches!(b.as_str(), "up" | "down" | "left" | "right"))
        {
            return Err(N64Error::BadParams(
                "stick aliases and explicit axes cannot be combined".into(),
            ));
        }
        Ok(())
    }
    pub fn overlay(&self, pulse: Self) -> N64Result<Self> {
        let state = Self {
            buttons: self.buttons.union(&pulse.buttons).cloned().collect(),
            axes: if pulse.axes.is_empty() {
                self.axes.clone()
            } else {
                pulse.axes
            },
            engaged: true,
        };
        state.validate()?;
        Ok(state)
    }
    pub fn raw(&self) -> (u32, i32, i32) {
        let mut bits = 0;
        for (button, bit) in [
            ("dpad_right", 0),
            ("dpad_left", 1),
            ("dpad_down", 2),
            ("dpad_up", 3),
            ("start", 4),
            ("z", 5),
            ("b", 6),
            ("a", 7),
            ("c_right", 8),
            ("c_left", 9),
            ("c_down", 10),
            ("c_up", 11),
            ("r", 12),
            ("l", 13),
        ] {
            if self.buttons.contains(button) {
                bits |= 1 << bit;
            }
        }
        let x = if self.buttons.contains("right") {
            1
        } else if self.buttons.contains("left") {
            -1
        } else {
            0
        };
        let y = if self.buttons.contains("down") {
            -1
        } else if self.buttons.contains("up") {
            1
        } else {
            0
        };
        let scale = if x != 0 && y != 0 { 56 } else { 80 };
        (
            bits,
            self.axes
                .get("stick_x")
                .copied()
                .unwrap_or((x * scale) as i64) as i32,
            self.axes
                .get("stick_y")
                .copied()
                .unwrap_or((y * scale) as i64) as i32,
        )
    }
    pub fn public(&self) -> Value {
        json!({"engaged":self.engaged,"buttons":self.buttons,"axes":self.axes,
            "ownership":if self.engaged {"override"} else {"native"}})
    }
}

pub(super) fn axes_capability() -> Value {
    json!({"semantics":"complete_persistent_controller_state","ports":[{"port":0,"axes":{
        "stick_x":{"minimum":-128,"neutral":0,"maximum":127,"negative_direction":"left","positive_direction":"right"},
        "stick_y":{"minimum":-128,"neutral":0,"maximum":127,"negative_direction":"down","positive_direction":"up"}}}],
        "notes":"Raw signed controller bytes. Stick aliases use 80 on one axis and 56 diagonally. Explicit zero axes hold neutral; empty buttons and axes return native input. Persistent input survives load/reset."})
}
impl Mupen64PlusHost {
    pub(super) fn publish_controller(&mut self, state: ControllerState) -> N64Result<()> {
        let (buttons, x, y) = state.raw();
        check_core("CoreEmucapInput", unsafe {
            (self.api.core_emucap_input)(1, 0, u32::from(state.engaged), buttons, x, y)
        })?;
        self.controller = state;
        Ok(())
    }
    pub(super) fn validate_input(&self, params: &Value) -> N64Result<ControllerState> {
        let state = ControllerState::decode(params)?;
        if params.get("_input_scope").is_some_and(|v| !v.is_string()) {
            return Err(N64Error::BadParams("input scope must be a string".into()));
        }
        match params.get("_input_scope").and_then(Value::as_str) {
            None if self.input_baseline.is_none() => Ok(state),
            None => Err(N64Error::Busy),
            Some("validate" | "acquire") if self.input_baseline.is_none() => {
                self.controller.overlay(state)
            }
            Some("release") => Ok(state),
            Some("validate" | "acquire") => Err(N64Error::Busy),
            _ => Err(N64Error::BadParams("invalid input scope operation".into())),
        }
    }
    pub(super) fn restore_input_baseline(&mut self) -> N64Result<()> {
        if let Some((_, baseline)) = self.input_baseline.clone() {
            self.publish_controller(baseline)?;
            self.input_baseline = None;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "n64_adapter_input_tests.rs"]
mod tests;
