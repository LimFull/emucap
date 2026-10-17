use super::plan::adapter_log_path;
use super::*;
use crate::args::LaunchExecutionProfileArgs;

/// Mednafen leg of `make_launch` (Saturn/PSX/PCE/MD): resolve the built fork (generation copy unless
/// MEDNAFEN_BIN is pinned) and hand off with the force_module.
pub(super) fn launch_mednafen(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    module: Option<&'static str>,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repository root was not found; set EMUCAP_REPO_ROOT" });
    };
    let Some((binary, explicit)) = emucap::launch::mednafen::resolve_binary(&root) else {
        return serde_json::json!({ "launched": false, "reason": "Mednafen binary was not found; build it with adapters/mednafen/build.sh or set MEDNAFEN_BIN" });
    };
    let log = match adapter_log_path(port, runtime, "mednafen.log") {
        Ok(path) => path,
        Err(error) => return serde_json::json!({"launched": false, "error": error.to_string()}),
    };
    let host_build = emucap::launch::mednafen::read_build_metadata(&binary)
        .ok()
        .flatten();
    let sound = a.sound.unwrap_or(false);
    let display = a.display.unwrap_or(false);
    let repeatable = a.execution_profile == Some(LaunchExecutionProfileArgs::Repeatable);
    let runtime_home = match emucap::launch::mednafen::managed_runtime_home(port, runtime) {
        Ok(path) => path,
        Err(error) => return serde_json::json!({"launched": false, "error": error.to_string()}),
    };
    let spec = emucap::launch::mednafen::Launch {
        binary: &binary,
        explicit_binary: explicit,
        content: &a.content_path,
        module,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
        headless: !display,
        sound,
        start_frozen: a.start_frozen,
        repeatable,
    };
    match emucap::launch::mednafen::launch(&spec) {
        Ok(pid) => serde_json::json!({
            "launched": true,
            "adapter": "mednafen",
            "module": module,
            "display": display,
            "sound": sound,
            "pid": pid,
            "port": port,
            "binary": binary.display().to_string(),
            "host_build": host_build,
            "log": log.display().to_string(),
            "emucap_home": runtime_home.display().to_string(),
            "isolation": "Mednafen uses an emucap-owned launch-generation home and copies known BIOS files from the shared emucap firmware inventory; the user's ~/.mednafen profile is not read or changed.",
            "execution_profile": repeatable.then(|| emucap::live::runtime::ExecutionProfileIdentity {
                id: emucap::launch::mednafen::MD_REPEATABLE_PROFILE_ID.into(),
                conditions_sha256: emucap::launch::mednafen::MD_REPEATABLE_CONDITIONS_SHA256.into(),
            }),
            "next_action": "launch returns after the adapter connects",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}
