//! Finite controller state application. The producer owns the baseline and its lifetime.
use super::*;
use std::collections::BTreeMap;

pub fn tap_axes(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    axes: &BTreeMap<String, i64>,
    press_frames: u64,
    after_frames: u64,
) -> Result<ToolOutput, LinkError> {
    tap_axes_in_unit(
        link,
        port,
        buttons,
        axes,
        press_frames,
        after_frames,
        StepUnit::Frames,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn tap_axes_in_unit(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    axes: &BTreeMap<String, i64>,
    press_frames: u64,
    after_frames: u64,
    unit: StepUnit,
) -> Result<ToolOutput, LinkError> {
    if !matches!(unit, StepUnit::Frames | StepUnit::InputPolls)
        || (unit != StepUnit::Frames
            && !link
                .capabilities()
                .methods
                .iter()
                .any(|m| m == unit.capability_method()))
    {
        return Err(bad_params("requested tap clock is unavailable"));
    }
    let advance = |link: &mut dyn EmulatorLink, count| {
        link.call(
            unit.capability_method(),
            if unit == StepUnit::Frames {
                json!({"frames":count})
            } else {
                json!({"count":count})
            },
        )
    };
    validate_sync_advance("tap press frame", press_frames)?;
    validate_sync_advance("tap trailing frame", after_frames)?;
    if press_frames == 0 {
        return Err(LinkError::Emulator {
            kind: "bad_params".into(),
            message: "press_frames must be positive".into(),
        });
    }
    let scoped = link
        .capabilities()
        .identity
        .host_features
        .iter()
        .any(|f| f == "scoped_controller_input");
    if scoped
        && !link
            .capabilities()
            .methods
            .iter()
            .any(|m| m == unit.capability_method())
    {
        return Err(bad_params("requested tap clock is unavailable"));
    }
    if !scoped {
        if !axes.is_empty() || unit != StepUnit::Frames {
            return Err(LinkError::Emulator {
                kind: "unsupported".into(),
                message: "timed controller axes are not advertised by this runtime".into(),
            });
        }
        return super::tap_buttons(link, port, buttons, press_frames, after_frames);
    }
    let mut request = json!({"port":port,"buttons":buttons,"axes":axes,"_input_scope":"validate"});
    link.call("set_input", request.clone())?;
    link.call("pause", json!({}))?;
    let launch = link.capabilities().identity.launch_id.clone();
    request["_input_scope"] = json!("acquire");
    let outcome = link
        .call("set_input", request)
        .and_then(|_| advance(link, press_frames));
    let release = call_session_cleanup(
        link,
        "set_input",
        json!({"port":port,"buttons":[],"_input_scope":"release"}),
        launch.as_deref(),
    )
    .map(|_| ());
    let outcome = finish_with_cleanup(outcome, release, combine_temporal_cleanup_error);
    let pressed = finish_frozen(link, launch.as_deref(), outcome)?;
    if pressed["status"] == "interrupted" {
        let mut reply = pressed;
        reply["operation"] = json!("tap");
        reply["phase"] = json!("press");
        return Ok(ToolOutput::Json(reply));
    }
    if after_frames > 0 {
        let outcome = advance(link, after_frames);
        let mut after = finish_frozen(link, launch.as_deref(), outcome)?;
        if after["status"] == "interrupted" {
            after["operation"] = json!("tap");
            after["phase"] = json!("after_release");
            return Ok(ToolOutput::Json(after));
        }
    }
    let mut result =
        json!({"tapped":buttons,"axes":axes,"state":"frozen","transient_input_released":true});
    if unit == StepUnit::Frames {
        result["press_frames"] = json!(press_frames);
        result["after_frames"] = json!(after_frames);
    } else {
        result["timing"] = json!({"unit":unit.as_str(),"press":press_frames,"after":after_frames});
    }
    Ok(ToolOutput::Json(result))
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
