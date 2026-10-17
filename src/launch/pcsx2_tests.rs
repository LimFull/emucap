use super::*;
use crate::test_env::{lock_env, EnvGuard};

fn launch_for<'a>(binary: &'a Path, bridge: &'a Path, bios: &'a Path, log: &'a Path) -> Launch<'a> {
    Launch {
        binary,
        bridge,
        bios,
        content: "/games/test.iso",
        log_path: log,
        port: 47870,
        name: Some("ps2-test"),
        session_token: Some("token"),
        runtime: None,
        display: false,
    }
}

#[test]
fn session_config_enables_pine_and_uses_bios_in_place() {
    let _lock = lock_env();
    let _guard = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let temporary = tempfile::tempdir().unwrap();
    let emulator_home = temporary.path().join("emulators");
    let bios_directory = temporary.path().join("operator-bios");
    std::fs::create_dir_all(&bios_directory).unwrap();
    let bios = bios_directory.join("SCPH-10000.bin");
    std::fs::write(&bios, vec![0u8; 4 * 1024 * 1024]).unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", &emulator_home);

    let prepared = prepare_session(47870, &bios, 39001, None).unwrap();
    let ini = std::fs::read_to_string(prepared.data_root.join("inis/PCSX2.ini")).unwrap();
    assert!(ini.contains("SetupWizardIncomplete = false"));
    assert!(ini.contains("Language = en-US"));
    assert!(ini.contains("EnablePINE = true"));
    assert!(ini.contains("PINESlot = 39001"));
    assert!(ini.contains(&format!("Bios = {}", bios_directory.display())));
    assert!(ini.contains("BIOS = SCPH-10000.bin"));
    assert!(!prepared.data_root.join("bios/SCPH-10000.bin").exists());
    assert!(prepared.data_root.join("memcards").is_dir());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for directory in [
            emulator_home,
            prepared.home,
            prepared.data_root.clone(),
            prepared.data_root.join("inis"),
            prepared.data_root.join("memcards"),
            prepared.pine_runtime,
        ] {
            assert_eq!(
                std::fs::metadata(directory).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        assert_eq!(
            std::fs::metadata(prepared.data_root.join("inis/PCSX2.ini"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
}

#[test]
fn managed_sessions_do_not_inherit_memory_cards_across_launch_ids() {
    let _lock = lock_env();
    let _guard = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let temporary = tempfile::tempdir().unwrap();
    let emulator_home = temporary.path().join("emulators");
    std::env::set_var("EMUCAP_EMU_HOME", &emulator_home);
    let bios = temporary.path().join("bios.bin");
    std::fs::write(&bios, b"bios").unwrap();
    let store = crate::live::runtime::RuntimeStore::new(emulator_home.join("sessions"));

    let first = store.prepare(47870).unwrap();
    let first_failure = first.adapter_failure_path();
    let first_runtime = crate::launch::RuntimeEnv {
        launch_id: first.launch_id(),
        adapter_failure_path: &first_failure,
    };
    let first_session = prepare_session(47870, &bios, 39001, Some(first_runtime)).unwrap();
    std::fs::write(
        first_session.data_root.join("memcards/slot1.ps2"),
        b"first generation",
    )
    .unwrap();

    let second = store.prepare(47870).unwrap();
    let second_failure = second.adapter_failure_path();
    let second_runtime = crate::launch::RuntimeEnv {
        launch_id: second.launch_id(),
        adapter_failure_path: &second_failure,
    };
    let second_session = prepare_session(47870, &bios, 39001, Some(second_runtime)).unwrap();

    assert_ne!(first_session.data_root, second_session.data_root);
    assert!(!second_session.data_root.join("memcards/slot1.ps2").exists());
    assert_eq!(first_session.pine_runtime, second_session.pine_runtime);
}

#[cfg(unix)]
#[test]
fn session_preparation_rejects_an_unsafe_reusable_home() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = lock_env();
    let _guard = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let temporary = tempfile::tempdir().unwrap();
    let emulator_home = temporary.path().join("emulators");
    std::fs::create_dir(&emulator_home).unwrap();
    std::fs::set_permissions(&emulator_home, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bios = temporary.path().join("bios.bin");
    std::fs::write(&bios, b"bios").unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", &emulator_home);
    assert_eq!(
        prepare_session(47870, &bios, 39001, None)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

#[test]
fn emulator_spec_isolates_data_and_places_flags_before_content() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("pcsx2-qt");
    let bridge = dir.path().join("bridge");
    let bios = dir.path().join("bios.bin");
    let log = dir.path().join("pcsx2.log");
    let launch = launch_for(&binary, &bridge, &bios, &log);
    let prepared = PreparedSession {
        home: dir.path().join("home"),
        data_root: dir.path().join("data"),
        pine_runtime: dir.path().join("pine"),
        pine_socket: pine_socket_path(&dir.path().join("pine"), 39001),
    };
    let spec = emulator_spec(&launch, &prepared, 39001, "native-test-token");
    assert_eq!(
        spec.args,
        vec![
            "-batch",
            "-fastboot",
            "-nofullscreen",
            "-nogui",
            "--",
            "/games/test.iso"
        ]
    );
    assert_eq!(
        spec.env
            .iter()
            .find(|(key, _)| key == "EMUCAP_PCSX2_DATAROOT")
            .map(|(_, value)| PathBuf::from(value)),
        Some(prepared.data_root.clone())
    );
    assert_eq!(
        spec.env
            .iter()
            .find(|(key, _)| key == "EMUCAP_NATIVE_DEBUG_TOKEN")
            .map(|(_, value)| value.as_str()),
        Some("native-test-token")
    );
    let bridge_spec = bridge_spec(&launch, &prepared, 39001, "native-test-token");
    assert_eq!(
        bridge_spec
            .env
            .iter()
            .find(|(key, _)| key == "EMUCAP_PCSX2_DATAROOT")
            .map(|(_, value)| PathBuf::from(value)),
        Some(prepared.data_root.clone())
    );
    assert_eq!(
        bridge_spec
            .env
            .iter()
            .find(|(key, _)| key == "EMUCAP_PCSX2_CAPTURE_DIR")
            .map(|(_, value)| PathBuf::from(value)),
        Some(prepared.data_root.join("captures"))
    );
    assert_eq!(
        bridge_spec
            .env
            .iter()
            .find(|(key, _)| key == "EMUCAP_NATIVE_DEBUG_TOKEN")
            .map(|(_, value)| value.as_str()),
        Some("native-test-token")
    );
}

#[test]
fn build_sidecar_must_match_the_pinned_lock() {
    let root = tempfile::tempdir().unwrap();
    let adapter = root.path().join("adapters/pcsx2");
    let bin = root.path().join("build/pcsx2-qt");
    std::fs::create_dir_all(&adapter).unwrap();
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(
        adapter.join("upstream.lock"),
        "PCSX2_REPO=https://example.invalid/pcsx2.git\n\
         PCSX2_COMMIT=1111111111111111111111111111111111111111\n\
         PCSX2_PATCHES_REPO=https://example.invalid/pcsx2-patches.git\n\
         PCSX2_PATCHES_COMMIT=2222222222222222222222222222222222222222\n\
         PCSX2_PATCHES_TREE=3333333333333333333333333333333333333333\n\
         PCSX2_PATCHES_ARCHIVE_SHA256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
         PCSX2_HOST_API=9\n\
         PCSX2_PATCHSET_SHA256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    )
    .unwrap();
    std::fs::write(
        build_metadata_path(&bin),
        serde_json::to_vec(&BuildMetadata {
            upstream: "https://example.invalid/pcsx2.git".into(),
            commit: "1111111111111111111111111111111111111111".into(),
            patches_upstream: "https://example.invalid/pcsx2-patches.git".into(),
            patches_commit: "2222222222222222222222222222222222222222".into(),
            patches_tree: "3333333333333333333333333333333333333333".into(),
            patches_archive_sha256:
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            host_api: REQUIRED_HOST_API,
            patchset_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .into(),
        })
        .unwrap(),
    )
    .unwrap();
    assert!(require_compatible_build(root.path(), &bin).is_ok());

    let mut mismatch: BuildMetadata =
        serde_json::from_slice(&std::fs::read(build_metadata_path(&bin)).unwrap()).unwrap();
    mismatch.host_api = REQUIRED_HOST_API + 1;
    std::fs::write(
        build_metadata_path(&bin),
        serde_json::to_vec(&mismatch).unwrap(),
    )
    .unwrap();
    assert!(require_compatible_build(root.path(), &bin)
        .unwrap_err()
        .to_string()
        .contains(&format!(
            "host API {} is incompatible",
            REQUIRED_HOST_API + 1
        )));
}

#[test]
#[cfg(unix)]
fn pine_socket_paths_beyond_the_unix_limit_are_refused() {
    let short = pine_socket_path(Path::new("/a/emucap/pcsx2/47800/pine"), 47801).unwrap();
    assert!(unix_socket_path_fits(&short));
    let long_dir = format!("/{}", "d".repeat(MAX_UNIX_SOCKET_PATH));
    let long = pine_socket_path(Path::new(&long_dir), 47801).unwrap();
    assert!(!unix_socket_path_fits(&long));
}
