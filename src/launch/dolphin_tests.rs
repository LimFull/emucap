use super::*;
use crate::test_env::{lock_env, EnvGuard};

fn output_status(backend: &str, result: &str, state: &str) -> serde_json::Value {
    serde_json::json!({
        "state": state,
        "audio_output": {
            "stream_generation": "1", "backend": backend, "initialized": true,
            "phase": "ready", "last_run_result": result, "start_verified": true,
            "failure": null
        }
    })
}

#[test]
fn audio_admission_uses_actual_backend_and_accepts_frozen_stopped_output() {
    let started = output_status("native", "started", "running");
    let stopped = output_status("native", "stopped", "frozen");
    assert_eq!(audio_output_ready(&started, true), Ok(true));
    assert_eq!(audio_output_ready(&stopped, true), Ok(true));
    assert!(audio_output_ready(&started, false).is_err());
    let null = output_status("No Audio Output", "stopped", "frozen");
    assert_eq!(audio_output_ready(&null, false), Ok(true));
    assert!(audio_output_ready(&null, true).is_err());
    assert_eq!(
        audio_output_ready(&output_status("native", "stopped", "running"), true),
        Ok(false)
    );
}

#[test]
fn audio_admission_rejects_failures_and_missing_start_evidence() {
    let valid = output_status("native", "stopped", "frozen");
    for (field, value) in [
        ("initialized", serde_json::json!(false)),
        ("start_verified", serde_json::json!(false)),
        ("failure", serde_json::json!("stop_failed")),
        ("phase", serde_json::json!("absent")),
        ("last_run_result", serde_json::json!("failed")),
        ("stream_generation", serde_json::json!("0")),
        ("backend", serde_json::json!(null)),
    ] {
        let mut status = valid.clone();
        status["audio_output"][field] = value;
        assert!(audio_output_ready(&status, true).is_err(), "{field}");
    }
    for field in [
        "backend",
        "failure",
        "phase",
        "initialized",
        "start_verified",
        "stream_generation",
        "last_run_result",
    ] {
        let mut status = valid.clone();
        status["audio_output"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            audio_output_ready(&status, true).is_err(),
            "missing {field}"
        );
    }
    assert!(audio_output_ready(&serde_json::json!({}), true).is_err());
}

#[test]
fn audio_admission_waits_only_for_pending_native_evidence() {
    for phase in ["initializing", "starting", "stopping"] {
        let mut status = output_status("native", "unattempted", "frozen");
        status["audio_output"]["phase"] = serde_json::json!(phase);
        status["audio_output"]["start_verified"] = serde_json::json!(false);
        assert_eq!(audio_output_ready(&status, true), Ok(false));
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
fn make_private_root(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(windows)]
fn make_private_root(path: &Path) {
    crate::private_fs::windows::privatize_dir(path).unwrap();
}

#[cfg(not(any(unix, windows)))]
fn make_private_root(_path: &Path) {}

fn metadata() -> BuildMetadata {
    BuildMetadata {
        upstream: "https://github.com/dolphin-emu/dolphin.git".into(),
        commit: "1".repeat(40),
        host_api: REQUIRED_HOST_API,
        patchset_sha256: "2".repeat(64),
    }
}

#[test]
fn build_metadata_rejects_missing_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("Dolphin");
    std::fs::write(&binary, b"fake").unwrap();

    let error = read_build_metadata(&binary).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("dolphin-patch-required"));
}

#[test]
fn build_metadata_accepts_valid_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("Dolphin");
    std::fs::write(&binary, b"fake").unwrap();
    std::fs::write(
        build_metadata_path(&binary),
        serde_json::to_vec(&metadata()).unwrap(),
    )
    .unwrap();

    assert_eq!(read_build_metadata(&binary).unwrap(), metadata());
}

#[test]
fn headless_discovery_requires_the_headless_frontend() {
    let root = tempfile::tempdir().unwrap();
    let directory = root
        .path()
        .join("adapters/dolphin/work/dolphin-src/Binary/x64");
    std::fs::create_dir_all(&directory).unwrap();
    let gui = directory.join("Dolphin.exe");
    let headless = directory.join("DolphinNoGUI.exe");
    std::fs::write(&gui, b"gui").unwrap();
    #[cfg(unix)]
    make_executable(&gui);
    assert_eq!(
        super::super::first_existing_file(local_build_candidates(root.path(), false)),
        None
    );
    std::fs::write(&headless, b"headless").unwrap();
    #[cfg(unix)]
    make_executable(&headless);
    assert_eq!(
        super::super::first_existing_file(local_build_candidates(root.path(), false)),
        Some(headless)
    );
    assert_eq!(
        super::super::first_existing_file(local_build_candidates(root.path(), true)),
        Some(gui)
    );
}

#[test]
fn resolve_binary_accepts_explicit_app_bundle() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&[
        "EMUCAP_DOLPHIN_GUI_BIN",
        "EMUCAP_DOLPHIN_HEADLESS_BIN",
        "EMUCAP_DOLPHIN_BIN",
    ]);
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("DolphinQt.app");
    let binary = app.join("Contents/MacOS/DolphinQt");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"fake").unwrap();
    #[cfg(unix)]
    make_executable(&binary);

    std::env::set_var("EMUCAP_DOLPHIN_GUI_BIN", &app);

    assert_eq!(resolve_binary(dir.path(), true), Some(binary));
}

#[test]
fn runtime_plain_binary_is_copied_under_a_unique_manual_generation() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let source = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    make_private_root(emu_home.path());
    let binary = source.path().join("dolphin-emu-nogui");
    std::fs::write(&binary, b"fake dolphin").unwrap();
    std::fs::create_dir_all(source.path().join("Sys/GC")).unwrap();
    std::fs::write(source.path().join("Sys/GC/font.bin"), b"system font").unwrap();
    std::fs::create_dir_all(source.path().join("User")).unwrap();
    std::fs::write(source.path().join("User/settings.ini"), b"private settings").unwrap();
    std::fs::write(
        build_metadata_path(&binary),
        serde_json::to_vec(&metadata()).unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    make_executable(&binary);
    std::env::set_var("EMUCAP_EMU_HOME", emu_home.path());

    let prepared = prepare_runtime_binary(&binary, 47920).unwrap();

    assert!(prepared
        .home
        .starts_with(emu_home.path().join("dolphin/47920/manual")));
    assert!(prepared.binary.ends_with("runtime/dolphin-emu-nogui"));
    assert_eq!(prepared.user_dir, prepared.home.join("user"));
    assert!(prepared
        .home
        .join("runtime/emucap-dolphin-build.json")
        .is_file());
    assert_eq!(std::fs::read(&binary).unwrap(), b"fake dolphin");
    assert_eq!(
        std::fs::read(prepared.home.join("runtime/Sys/GC/font.bin")).unwrap(),
        b"system font"
    );
    assert!(!prepared.home.join("runtime/User").exists());
    assert_eq!(
        std::fs::read(source.path().join("User/settings.ini")).unwrap(),
        b"private settings"
    );
}

#[test]
fn managed_user_directories_do_not_cross_launch_generations() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let temporary = tempfile::tempdir().unwrap();
    let emulator_home = temporary.path().join("emulators");
    std::env::set_var("EMUCAP_EMU_HOME", &emulator_home);
    let binary = temporary.path().join("dolphin-emu-nogui");
    std::fs::write(&binary, b"fake dolphin").unwrap();
    #[cfg(unix)]
    make_executable(&binary);
    let store = crate::live::runtime::RuntimeStore::new(emulator_home.join("sessions"));

    let first = store.prepare(47920).unwrap();
    let first_failure = first.adapter_failure_path();
    let first_runtime = crate::launch::RuntimeEnv {
        launch_id: first.launch_id(),
        adapter_failure_path: &first_failure,
    };
    let first_prepared =
        prepare_runtime_binary_for_generation(&binary, 47920, Some(first_runtime)).unwrap();
    std::fs::write(
        first_prepared.user_dir.join("sentinel"),
        b"first generation",
    )
    .unwrap();

    let second = store.prepare(47920).unwrap();
    let second_failure = second.adapter_failure_path();
    let second_runtime = crate::launch::RuntimeEnv {
        launch_id: second.launch_id(),
        adapter_failure_path: &second_failure,
    };
    let second_prepared =
        prepare_runtime_binary_for_generation(&binary, 47920, Some(second_runtime)).unwrap();

    assert_ne!(first_prepared.user_dir, second_prepared.user_dir);
    assert!(!second_prepared.user_dir.join("sentinel").exists());
}

#[test]
fn runtime_windows_distribution_preserves_dependencies_and_system_data() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let source = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    make_private_root(home.path());
    std::env::set_var("EMUCAP_EMU_HOME", home.path());
    let files = [
        ("Dolphin.exe", "executable"),
        ("Qt6Core.dll", "dependency"),
        ("Sys/GC/font.bin", "system data"),
        ("QtPlugins/platforms/qwindows.dll", "platform plugin"),
        ("qt.conf", "[Paths]\nPlugins = QtPlugins"),
    ];
    for (name, content) in files {
        let path = source.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    std::fs::create_dir(source.path().join("User")).unwrap();
    std::fs::write(source.path().join("User/settings.ini"), "personal settings").unwrap();
    let prepared = prepare_runtime_binary(&source.path().join("Dolphin.exe"), 47925).unwrap();
    let runtime = prepared.binary.parent().unwrap();
    for (name, content) in files {
        assert_eq!(
            std::fs::read_to_string(runtime.join(name)).unwrap(),
            content
        );
    }
    assert!(!runtime.join("User").exists());
    assert_eq!(
        std::fs::read_to_string(source.path().join("User/settings.ini")).unwrap(),
        "personal settings"
    );
}

#[test]
fn runtime_app_bundle_copy_preserves_resources() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let source = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    make_private_root(emu_home.path());
    let app = source.path().join("DolphinQt.app");
    let binary = app.join("Contents/MacOS/DolphinQt");
    let resource = app.join("Contents/Resources/qt.conf");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::create_dir_all(resource.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"fake dolphin").unwrap();
    std::fs::write(&resource, b"resource").unwrap();
    #[cfg(unix)]
    make_executable(&binary);
    std::env::set_var("EMUCAP_EMU_HOME", emu_home.path());

    let prepared = prepare_runtime_binary(&binary, 47921).unwrap();

    assert!(prepared
        .binary
        .ends_with("runtime/DolphinQt.app/Contents/MacOS/DolphinQt"));
    assert_eq!(
        std::fs::read(
            prepared
                .home
                .join("runtime/DolphinQt.app/Contents/Resources/qt.conf")
        )
        .unwrap(),
        b"resource"
    );
}

#[cfg(unix)]
#[test]
fn runtime_refuses_symlinked_binary_inside_owned_home() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let outside = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    let target = outside.path().join("Dolphin");
    std::fs::write(&target, b"user dolphin").unwrap();
    make_executable(&target);
    let runtime = emu_home.path().join("dolphin/47922/runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let link = runtime.join("Dolphin");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", emu_home.path());

    let home = emu_home.path().join("dolphin/47922");
    let error = prepare_runtime_binary_in_home(&link, &home).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(std::fs::read(&target).unwrap(), b"user dolphin");
}

#[cfg(unix)]
#[test]
fn runtime_refuses_symlinked_user_directory_without_touching_target() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let source = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    let real_user = tempfile::tempdir().unwrap();
    let marker = real_user.path().join("Dolphin.ini");
    std::fs::write(&marker, b"user settings").unwrap();
    let binary = source.path().join("dolphin-emu-nogui");
    std::fs::write(&binary, b"fake dolphin").unwrap();
    make_executable(&binary);
    let home = emu_home.path().join("dolphin/47923");
    std::fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(real_user.path(), home.join("user")).unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", emu_home.path());

    let error = prepare_runtime_binary_in_home(&binary, &home).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(std::fs::read(marker).unwrap(), b"user settings");
}

#[cfg(unix)]
#[test]
fn runtime_refuses_symlinked_runtime_directory_without_touching_target() {
    let _guard = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let source = tempfile::tempdir().unwrap();
    let emu_home = tempfile::tempdir().unwrap();
    let installed = tempfile::tempdir().unwrap();
    let marker = installed.path().join("Dolphin");
    std::fs::write(&marker, b"installed dolphin").unwrap();
    let binary = source.path().join("dolphin-emu-nogui");
    std::fs::write(&binary, b"fake dolphin").unwrap();
    make_executable(&binary);
    let home = emu_home.path().join("dolphin/47924");
    std::fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(installed.path(), home.join("runtime")).unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", emu_home.path());

    let error = prepare_runtime_binary_in_home(&binary, &home).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(std::fs::read(marker).unwrap(), b"installed dolphin");
}
