use super::add;
use crate::args::{
    HoldTouchArgs, HoldUntilArgs, InputArgs, PressArgs, PulseTouchArgs, ReadInputPollsArgs,
    ReleaseTouchArgs, TouchArgs,
};
use crate::{analysis_surface, invalid_request_result, Emucap};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde_json::{Map, Value};

pub(crate) use emucap::live::controller_input::validate_controller_axes;

pub(super) fn describe(status: &Value, operations: &mut Map<String, Value>) {
    add::<ReadInputPollsArgs>(operations,status,"read_input_polls","Read bounded controller-response history after an exclusive sequence cursor. Check generation and dropped_before_cursor before joining records.");
    add::<InputArgs>(
        operations,
        status,
        "set_input",
        "Set button, key, or advertised controller-axis state across separate debugger calls. The caller owns release: empty buttons and axes release native ownership. For timed controller input, use tap.",
    );
    add::<HoldTouchArgs>(
        operations,
        status,
        "hold_touch",
        "Hold one touch-screen coordinate until release_touch or generation termination.",
    );
    add::<ReleaseTouchArgs>(
        operations,
        status,
        "release_touch",
        "Release persistent touch-screen ownership without advancing guest time.",
    );
    add::<PulseTouchArgs>(
        operations,
        status,
        "pulse_touch_while_running",
        "Hold one touch-screen coordinate for a real-time frame duration, release it, and leave guest execution running.",
    );
    add::<PressArgs>(
        operations,
        status,
        "pulse_while_running",
        "Hold buttons for a real-time frame duration, release them, and leave guest execution running.",
    );
    add::<HoldUntilArgs>(
        operations,
        status,
        "hold_until",
        "Advance while holding buttons until watched memory changes, then release and return frozen.",
    );
}

pub(crate) async fn execute(
    server: &Emucap,
    status: &Value,
    operation: &str,
    arguments: Option<Map<String, Value>>,
) -> CallToolResult {
    match operation {
        "read_input_polls" => {
            match analysis_surface::parse_arguments::<ReadInputPollsArgs>(operation, arguments) {
                Ok(a) => {
                    let mut link = match server.link.try_lock() {
                        Ok(link) => link,
                        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                        Err(std::sync::TryLockError::WouldBlock) => {
                            return crate::link_error_result(emucap::live::link::LinkError::Busy)
                        }
                    };
                    let mut params = serde_json::json!({"after_sequence":a.after_sequence});
                    if let Some(limit) = a.limit {
                        params["limit"] = serde_json::json!(limit);
                    }
                    match link.call("read_input_polls", params) {
                        Ok(value) => {
                            crate::tool_output_result(emucap::live::tools::ToolOutput::Json(value))
                        }
                        Err(error) => crate::link_error_result(error),
                    }
                }
                Err(error) => invalid_request_result(error),
            }
        }
        "set_input" => match analysis_surface::parse_arguments::<InputArgs>(operation, arguments) {
            Ok(values) => match validate_controller_axes(status, values.port, &values.axes) {
                Ok(()) => server.set_input(Parameters(values)).await,
                Err(error) => invalid_request_result(error),
            },
            Err(error) => invalid_request_result(error),
        },
        "hold_touch" => {
            match analysis_surface::parse_arguments::<HoldTouchArgs>(operation, arguments) {
                Ok(values) => {
                    server
                        .touch(Parameters(TouchArgs {
                            port: values.port,
                            x: Some(values.x),
                            y: Some(values.y),
                            frames: None,
                            release: false,
                        }))
                        .await
                }
                Err(error) => invalid_request_result(error),
            }
        }
        "release_touch" => {
            match analysis_surface::parse_arguments::<ReleaseTouchArgs>(operation, arguments) {
                Ok(values) => {
                    server
                        .touch(Parameters(TouchArgs {
                            port: values.port,
                            x: None,
                            y: None,
                            frames: None,
                            release: true,
                        }))
                        .await
                }
                Err(error) => invalid_request_result(error),
            }
        }
        "pulse_touch_while_running" => {
            match analysis_surface::parse_arguments::<PulseTouchArgs>(operation, arguments) {
                Ok(values) => {
                    server
                        .touch(Parameters(TouchArgs {
                            port: values.port,
                            x: Some(values.x),
                            y: Some(values.y),
                            frames: Some(values.frames),
                            release: false,
                        }))
                        .await
                }
                Err(error) => invalid_request_result(error),
            }
        }
        "pulse_while_running" => match analysis_surface::parse_arguments(operation, arguments) {
            Ok(values) => server.pulse_while_running(Parameters(values)).await,
            Err(error) => invalid_request_result(error),
        },
        "hold_until" => match analysis_surface::parse_arguments(operation, arguments) {
            Ok(values) => server.hold_until(Parameters(values)).await,
            Err(error) => invalid_request_result(error),
        },
        _ => invalid_request_result(format!("unknown debug input operation: {operation}")),
    }
}

#[cfg(test)]
#[path = "debug_input_tests.rs"]
mod tests;
