use super::*;
use crate::live::{runtime::RuntimeStore, tcp};
use crate::test_env::{lock_env, EnvGuard};
use std::process::Command;

fn unavailable() -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no user data root",
    ))
}

#[test]
fn explicit_root_overrides_unavailable_platform_root() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        resolve_emu_home_base(Some(root.path().as_os_str().to_owned()), unavailable()).unwrap(),
        root.path()
    );
}

#[test]
fn root_discovery_rejects_missing_empty_and_relative_roots() {
    assert_eq!(
        resolve_emu_home_base(None, unavailable())
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    for path in ["", "relative/root", "../emucap"] {
        assert_eq!(
            resolve_emu_home_base(None, Ok(PathBuf::from(path)))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(resolve_emu_home_base(Some(path.into()), unavailable()).is_err());
    }
}

#[test]
fn managed_emulator_profiles_are_disjoint_generation_members() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let temporary = tempfile::tempdir().unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", temporary.path());
    let store = RuntimeStore::new(temporary.path().join("sessions"));
    let first = store.prepare(47800).unwrap();
    let first_failure = first.adapter_failure_path();
    let first_runtime = RuntimeEnv {
        launch_id: first.launch_id(),
        adapter_failure_path: &first_failure,
    };
    let first_profile = emu_generation_dir("ppsspp", 47800, Some(first_runtime)).unwrap();
    std::fs::write(first_profile.join("sentinel"), b"first generation").unwrap();

    let second = store.prepare(47800).unwrap();
    let second_failure = second.adapter_failure_path();
    let second_runtime = RuntimeEnv {
        launch_id: second.launch_id(),
        adapter_failure_path: &second_failure,
    };
    let second_profile = emu_generation_dir("ppsspp", 47800, Some(second_runtime)).unwrap();

    assert_eq!(
        first_profile,
        first.generation_dir().join("emulators/ppsspp")
    );
    assert_eq!(
        second_profile,
        second.generation_dir().join("emulators/ppsspp")
    );
    assert_ne!(first_profile, second_profile);
    assert!(!second_profile.join("sentinel").exists());
    assert_eq!(
        std::fs::read(first_profile.join("sentinel")).unwrap(),
        b"first generation"
    );
    assert_eq!(
        runtime_generation_dir(47801, first_runtime)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    second.abort().unwrap();
    assert!(!second_profile.exists());
    assert!(first_profile.join("sentinel").is_file());
}

#[test]
fn managed_emulator_profile_rejects_a_mismatched_generation_path() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let temporary = tempfile::tempdir().unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", temporary.path());
    let store = RuntimeStore::new(temporary.path().join("sessions"));
    let prepared = store.prepare(47800).unwrap();
    let failure = prepared.adapter_failure_path();
    let runtime = RuntimeEnv {
        launch_id: "launch-another-generation",
        adapter_failure_path: &failure,
    };

    assert_eq!(
        emu_generation_dir("pcsx2", 47800, Some(runtime))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert!(!prepared.generation_dir().join("emulators").exists());
}

fn child(mode: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "launch::home_tests::root_discovery_child",
        "--nocapture",
    ])
    .env("EMUCAP_ROOT_TEST_CHILD", mode)
    .env_remove("EMUCAP_EMU_HOME")
    .env_remove("HOME")
    .env_remove("XDG_DATA_HOME")
    .env_remove("LOCALAPPDATA")
    .env_remove("EMUCAP_FLYCAST_BUILD_HOME")
    .env_remove("FLYCAST_APP");
    cmd
}

fn run_child(mut cmd: Command) -> String {
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn root_discovery_child() {
    let Ok(mode) = std::env::var("EMUCAP_ROOT_TEST_CHILD") else {
        return;
    };
    match mode.as_str() {
        "invalid" => {
            assert!(emu_home_base().is_err());
            assert!(emu_home_dir("flycast", 47800).is_err());
            assert!(RuntimeStore::discover().is_err());
            assert!(tcp::lazy("127.0.0.1:0", std::time::Duration::from_secs(1)).is_err());
            assert!(tcp::session_token_path(47800).is_err());
            assert!(flycast::build_home().is_err());
            assert!(flycast::resolve_binary().is_none());
        }
        "valid" => {
            let base = emu_home_base().unwrap();
            assert!(base.is_absolute());
            let store = RuntimeStore::discover().unwrap();
            assert_eq!(store.root(), base.join("sessions"));
            assert_eq!(
                tcp::session_token_path(47800).unwrap(),
                store.compatibility_token_path(47800)
            );
            assert_eq!(
                emu_home_dir("flycast", 47800).unwrap(),
                base.join("flycast/47800")
            );
            assert_eq!(flycast::build_home().unwrap(), base.join("flycast-build"));
            if let Some(planted) = std::env::var_os("EMUCAP_ROOT_TEST_PLANTED") {
                let resolved =
                    flycast::resolve_binary().and_then(|path| std::fs::canonicalize(path).ok());
                assert_ne!(resolved, Some(std::fs::canonicalize(planted).unwrap()));
            }
            println!("ROOT={}", base.display());
        }
        "implicit_binary_roots" => {
            assert!(flycast::default_install_candidates()
                .iter()
                .all(|path| path.is_absolute()));
        }
        _ => panic!("invalid test mode"),
    }
}

#[test]
fn invalid_root_fails_before_runtime_or_binary_discovery() {
    for root in ["", "relative/root"] {
        let mut cmd = child("invalid");
        cmd.env("EMUCAP_EMU_HOME", root);
        run_child(cmd);
    }
}

#[test]
fn explicit_root_is_shared_by_runtime_and_launcher_without_home() {
    let root = tempfile::tempdir().unwrap();
    let mut cmd = child("valid");
    cmd.env("EMUCAP_EMU_HOME", root.path());
    assert!(run_child(cmd).contains(&format!("ROOT={}", root.path().display())));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn implicit_flycast_install_roots_never_produce_relative_candidates() {
    for value in ["", "relative"] {
        let mut cmd = child("implicit_binary_roots");
        for key in [
            "HOME",
            "LOCALAPPDATA",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "USERPROFILE",
        ] {
            cmd.env(key, value);
        }
        run_child(cmd);
    }
}

#[cfg(unix)]
#[test]
fn missing_home_uses_same_account_root_across_processes_not_temp_binary() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let planted = temp
        .path()
        .join("emucap/flycast-build/work/build/Flycast.app/Contents/MacOS/Flycast");
    std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
    std::fs::write(&planted, b"#!/bin/sh\nexit 99\n").unwrap();
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = || {
        let mut cmd = child("valid");
        cmd.env("TMPDIR", temp.path())
            .env("EMUCAP_ROOT_TEST_PLANTED", &planted);
        run_child(cmd)
            .lines()
            .find_map(|line| line.strip_prefix("ROOT=").map(str::to_owned))
            .unwrap()
    };
    let first = run();
    assert_eq!(first, run());
    assert!(!Path::new(&first).starts_with(temp.path()));
    let shell = Command::new("bash")
        .args([
            "-c",
            ". adapters/_common/runtime-env.sh; emucap_runtime_base",
        ])
        .env_remove("HOME")
        .env_remove("EMUCAP_EMU_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();
    assert!(shell.status.success());
    assert_eq!(String::from_utf8(shell.stdout).unwrap().trim(), first);
}

#[cfg(windows)]
#[test]
fn missing_windows_local_data_root_fails_without_temp_fallback() {
    run_child(child("invalid"));
}

#[cfg(unix)]
#[test]
fn empty_or_relative_home_never_selects_a_cwd_legacy_flycast() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    for home in ["", "relative"] {
        let planted = temp
            .path()
            .join(home)
            .join("flycast/build/Flycast.app/Contents/MacOS/Flycast");
        std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
        std::fs::write(&planted, b"#!/bin/sh\nexit 99\n").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cmd = child("valid");
        cmd.current_dir(temp.path())
            .env("HOME", home)
            .env("EMUCAP_EMU_HOME", temp.path().join("data"))
            .env("EMUCAP_ROOT_TEST_PLANTED", &planted);
        run_child(cmd);
    }
}
