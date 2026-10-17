use serde::de::DeserializeOwned;

use crate::args::{RegressionRunArgs, VerifyDeterminismArgs};

pub(crate) fn describe() -> serde_json::Value {
    serde_json::json!({
        "surface": "analysis",
        "execution_owner": "current_control_session",
        "limits": {
            "suite_cases": emucap::analysis::regression::MAX_SUITE_CASES,
            "suite_entries": emucap::analysis::regression::MAX_SUITE_ENTRIES,
            "suite_case_json_bytes": emucap::analysis::regression::MAX_SUITE_CASE_JSON_BYTES,
            "suite_movie_bytes": emucap::analysis::regression::MAX_SUITE_MOVIE_BYTES,
            "movie_bytes": emucap::analysis::regression::MAX_REGRESSION_MOVIE_BYTES,
            "movie_rows": emucap::analysis::regression::MAX_REGRESSION_MOVIE_ROWS,
            "guest_advances": emucap::analysis::regression::MAX_REGRESSION_ADVANCES,
            "determinism_guest_advances": emucap::analysis::regression::MAX_DETERMINISM_ADVANCES,
            "operation_ms": emucap::live::temporal::MAX_SYNC_OPERATION_MS,
        },
        "operations": {
            "regression_run": {
                "description": "Replay a regression suite and return pass, fail, or invalid buckets.",
                "arguments_schema": schemars::schema_for!(RegressionRunArgs),
            },
            "verify_determinism": {
                "description": "Replay one case and compare observation hashes.",
                "arguments_schema": schemars::schema_for!(VerifyDeterminismArgs),
            }
        },
        "next_action": {
            "tool": "analysis",
            "arguments": {
                "operation": "regression_run | verify_determinism",
                "arguments": "Use the selected arguments_schema exactly."
            }
        }
    })
}

pub(crate) fn parse_arguments<T: DeserializeOwned>(
    operation: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::Object(arguments.unwrap_or_default()))
        .map_err(|error| format!("invalid {operation} arguments: {error}"))
}
