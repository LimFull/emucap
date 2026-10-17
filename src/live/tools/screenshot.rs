use std::path::Path;

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{EmulatorLink, LinkError, ToolOutput};

pub fn screenshot(
    link: &mut dyn EmulatorLink,
    save_path: Option<&Path>,
) -> Result<ToolOutput, LinkError> {
    let result = link.call("screenshot", json!({}))?;
    let b64 = result
        .get("png_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LinkError::Protocol("screenshot response has no png_base64 field".into()))?
        .to_string();

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| LinkError::Protocol(format!("failed to decode base64 screenshot: {e}")))?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    if let Some(reported) = result.get("sha256").and_then(Value::as_str) {
        if reported != sha256 {
            return Err(LinkError::Protocol(format!(
                "screenshot sha256 mismatch: adapter={reported}, decoded={sha256}"
            )));
        }
    }
    let saved_path = match save_path {
        Some(p) => {
            crate::path_safety::atomic_write_file(p, &bytes)
                .map_err(|e| LinkError::Protocol(format!("failed to save screenshot: {e}")))?;
            Some(p.to_string_lossy().to_string())
        }
        None => None,
    };
    let mut provenance = result.as_object().cloned().unwrap_or_default();
    provenance.remove("png_base64");
    provenance.insert("sha256".into(), Value::String(sha256));
    provenance.insert("byte_len".into(), json!(bytes.len()));
    Ok(ToolOutput::Image {
        png_base64: b64,
        saved_path,
        provenance: Value::Object(provenance),
    })
}
