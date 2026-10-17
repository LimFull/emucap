//! Validation against the connected producer controller advertisement.
use serde_json::Value;
use std::collections::BTreeMap;

pub fn validate_controller_axes(
    status: &Value,
    port: u64,
    axes: &BTreeMap<String, i64>,
) -> Result<(), String> {
    if axes.is_empty() {
        return Ok(());
    }
    let ports = status
        .pointer("/input_axes/ports")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "controller axes are not advertised by the current runtime; read full status for available controls".to_string()
        })?;
    let matching_ports = ports
        .iter()
        .filter(|entry| entry.get("port").and_then(Value::as_u64) == Some(port))
        .collect::<Vec<_>>();
    if matching_ports.len() != 1 {
        return Err(format!(
            "controller axes are not advertised for port {port}; use an advertised port"
        ));
    }
    let advertised = matching_ports[0]
        .get("axes")
        .and_then(Value::as_object)
        .ok_or_else(|| "the current runtime advertised malformed controller axes".to_string())?;
    for (name, value) in axes {
        let specification = advertised
            .get(name)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("controller axis {name} is not advertised for port {port}"))?;
        let minimum = specification
            .get("minimum")
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("controller axis {name} has no valid minimum"))?;
        let maximum = specification
            .get("maximum")
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("controller axis {name} has no valid maximum"))?;
        if minimum > maximum {
            return Err(format!(
                "controller axis {name} advertises an invalid range {minimum}..{maximum}"
            ));
        }
        if *value < minimum || *value > maximum {
            return Err(format!(
                "controller axis {name} value {value} is outside the advertised range {minimum}..{maximum}"
            ));
        }
    }
    Ok(())
}
