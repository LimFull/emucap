use super::*;

pub(super) fn precondition(root: &Path) -> serde_json::Value {
    let frontend = azahar_launch::resolve_frontend(root);
    let core = azahar_launch::require_compatible_core(root);
    serde_json::json!({
        "available": frontend.is_some() && core.is_ok(),
        "path": frontend.map(|path| path.display().to_string()),
        "core": core.as_ref().ok().map(|bundle| bundle.path.display().to_string()),
        "host_build": core.as_ref().ok().map(|bundle| &bundle.identity),
        "core_error": core.err().map(|error| error.to_string()),
        "source": "EMUCAP_AZAHAR_FRONTEND / installed emucap-azahar; EMUCAP_AZAHAR_BUILD_INFO / pinned repo build",
    })
}

/// Nintendo 3DS leg of `make_launch`: the direct host loads the pinned Azahar libretro core.
pub(super) fn launch_azahar(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    args: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({"launched":false, "error":"emucap repository root was not found; set EMUCAP_REPO_ROOT"});
    };
    let request = azahar_launch::Launch {
        repo_root: &root,
        content: Path::new(&args.content_path),
        port,
        name: args.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
    };
    match azahar_launch::launch(&request) {
        Ok(launched) => serde_json::json!({
            "launched": true,
            "adapter": "azahar_3ds",
            "backend": "azahar-libretro",
            "pid": launched.pid,
            "port": port,
            "display": false,
            "sound": false,
            "binary": launched.frontend.display().to_string(),
            "core": launched.core.display().to_string(),
            "runtime_home": launched.runtime_home.display().to_string(),
            "log": launched.log.display().to_string(),
            "next_action": "launch returns after the adapter connects"
        }),
        Err(error) => serde_json::json!({"launched":false, "error":error.to_string()}),
    }
}
