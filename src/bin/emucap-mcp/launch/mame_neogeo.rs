use super::plan::adapter_log_path;
use super::*;

/// Neo Geo MVS/AES/CD and Taito F2 leg of `make_launch`: spawn isolated MAME and the shared
/// 68000 bridge.
pub(super) fn launch_mame_neogeo(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    system: &str,
    args: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repo root not found; set EMUCAP_REPO_ROOT" });
    };
    let content = Path::new(&args.content_path);
    let taitof2 = system == "taitof2";
    let binary = if taitof2 {
        mame_neogeo_launch::resolve_taitof2_binary(&root)
    } else {
        mame_neogeo_launch::resolve_binary(&root)
    };
    let Some(binary) = binary else {
        return if taitof2 {
            serde_json::json!({ "launched": false, "reason": "Taito F2 MAME binary not found; run adapters/mame-taitof2/build.sh or set EMUCAP_TAITOF2_MAME_BIN" })
        } else {
            serde_json::json!({ "launched": false, "reason": "Neo Geo MAME binary not found; run adapters/mame-neogeo/build.sh or set EMUCAP_NEOGEO_MAME_BIN" })
        };
    };
    let Some(bridge) = mame_neogeo_launch::resolve_bridge(&root) else {
        return serde_json::json!({ "launched": false, "reason": "Neo Geo bridge binary not found; build emucap-mame-neogeo-bridge or set EMUCAP_NEOGEO_BRIDGE_BIN" });
    };
    let bios = mame_neogeo_launch::resolve_bios(content, system);
    if bios.is_none() && !taitof2 {
        return match system {
            "neogeo_aes" => serde_json::json!({
                "launched": false,
                "reason": "Neo Geo AES BIOS aes.zip not found",
                "next_action": "set EMUCAP_NEOGEO_AES_BIOS to aes.zip or place it beside the cartridge set",
            }),
            "neogeo_cd" => serde_json::json!({
                "launched": false,
                "reason": "Neo Geo CDZ BIOS neocdz.zip not found",
                "next_action": "set EMUCAP_NEOGEO_CD_BIOS to neocdz.zip or place it beside the CUE",
            }),
            _ => serde_json::json!({
                "launched": false,
                "reason": "Neo Geo MVS BIOS neogeo.zip not found",
                "next_action": "set EMUCAP_NEOGEO_BIOS to neogeo.zip or place it beside the game ROM set",
            }),
        };
    }
    let log_name = if taitof2 {
        "mame-taitof2.log"
    } else {
        "mame-neogeo.log"
    };
    let log = match adapter_log_path(port, runtime, log_name) {
        Ok(path) => path,
        Err(error) => return serde_json::json!({"launched": false, "error": error.to_string()}),
    };
    let display = args.display.unwrap_or(false);
    let launch = mame_neogeo_launch::Launch {
        binary: &binary,
        bridge: &bridge,
        repo_root: &root,
        content,
        bios: bios.as_deref(),
        system,
        log_path: &log,
        port,
        name: args.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
        display,
    };
    match mame_neogeo_launch::launch(&launch) {
        Ok(launched) => serde_json::json!({
            "launched": true,
            "adapter": if taitof2 { "mame_taitof2" } else { "mame_neogeo" },
            "pid": launched.mame_pid,
            "mame_pid": launched.mame_pid,
            "bridge_pid": launched.bridge_pid,
            "display": display,
            "driver": launched.driver,
            "gdb_port": launched.gdb_port,
            "port": port,
            "binary": binary.display().to_string(),
            "bios": bios.as_ref().map(|path| path.display().to_string()),
            "log": log.display().to_string(),
            "next_action": "launch returns after the adapter connects",
        }),
        Err(error) => serde_json::json!({ "launched": false, "error": error.to_string() }),
    }
}
