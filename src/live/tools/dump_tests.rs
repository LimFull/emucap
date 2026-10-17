use super::*;
use crate::live::link::{Capabilities, FakeLink};

#[test]
fn existing_empty_unrelated_and_prior_dump_directories_are_rejected_before_capture() {
    let root = tempfile::tempdir().unwrap();
    for contents in [None, Some("unrelated.txt"), Some("regions.json")] {
        let out = root.path().join("out");
        std::fs::create_dir(&out).unwrap();
        if let Some(name) = contents {
            std::fs::write(out.join(name), b"KEEP").unwrap();
        }
        let mut link = FakeLink::ok(json!({}));
        let error = dump_memory(&mut link, out.to_str().unwrap()).unwrap_err();
        assert!(error.to_string().contains("already exists"));
        assert!(link.last_method.is_none());
        if let Some(name) = contents {
            assert_eq!(std::fs::read(out.join(name)).unwrap(), b"KEEP");
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        std::fs::remove_dir_all(out).unwrap();
    }
}

#[test]
fn absent_destination_publishes_complete_staging_and_releases_ownership() {
    let root = tempfile::tempdir().unwrap();
    let out = root
        .path()
        .join("missing/parent/dump with spaces $() ; ' literal");
    let mut staging = DumpStaging::new(&out).unwrap();
    let payload = staging.path().to_owned();
    std::fs::write(payload.join("main.bin"), b"memory").unwrap();
    staging
        .payload
        .as_ref()
        .unwrap()
        .write_new_file("state.json", b"{}")
        .unwrap();
    staging.publish().unwrap();
    staging.cleanup().unwrap();
    drop(staging);
    assert_eq!(std::fs::read(out.join("main.bin")).unwrap(), b"memory");
    assert_eq!(std::fs::read(out.join("state.json")).unwrap(), b"{}");
    assert!(!payload.exists());
    assert_eq!(std::fs::read_dir(out.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn publication_never_replaces_a_destination_created_after_admission() {
    let root = tempfile::tempdir().unwrap();
    for kind in ["empty", "directory", "file"] {
        let out = root.path().join(kind);
        let mut staging = DumpStaging::new(&out).unwrap();
        std::fs::write(staging.path().join("new.bin"), b"new").unwrap();
        match kind {
            "file" => std::fs::write(&out, b"KEEP").unwrap(),
            _ => {
                std::fs::create_dir(&out).unwrap();
                if kind == "directory" {
                    std::fs::write(out.join("sentinel"), b"KEEP").unwrap();
                }
            }
        }
        assert!(staging.publish().is_err());
        staging.cleanup().unwrap();
        if kind == "file" {
            assert_eq!(std::fs::read(&out).unwrap(), b"KEEP");
        } else if kind == "directory" {
            assert_eq!(std::fs::read(out.join("sentinel")).unwrap(), b"KEEP");
        } else {
            assert_eq!(std::fs::read_dir(out).unwrap().count(), 0);
        }
    }
}

#[test]
fn failed_directory_acquisition_never_claims_existing_entries() {
    let root = tempfile::tempdir().unwrap();
    let directory = Directory::open_parent(root.path()).unwrap();
    std::fs::create_dir(root.path().join("occupied")).unwrap();
    std::fs::write(root.path().join("occupied/keep"), b"KEEP").unwrap();
    assert!(directory.create_dir("occupied".as_ref()).is_err());
    assert_eq!(
        std::fs::read(root.path().join("occupied/keep")).unwrap(),
        b"KEEP"
    );
}

#[cfg(unix)]
#[test]
fn unacquired_created_entry_is_reported_without_deleting_it() {
    const CHILD: &str = "EMUCAP_DUMP_ACQUISITION_CHILD";
    if let Some(root) = std::env::var_os(CHILD) {
        // umask is process-global; change it only in this isolated test subprocess.
        let root = std::fs::canonicalize(root).unwrap();
        let previous = unsafe { libc::umask(0o700) };
        let error = DumpStaging::new(&root.join("dump")).err().unwrap();
        unsafe {
            libc::umask(previous);
        }
        let entries: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(entries.len(), 1);
        let leftover = entries[0].path();
        assert!(leftover
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(".dump-staging-"));
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&leftover, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            error
                .to_string()
                .contains(&format!("inspect {}", leftover.display())),
            "{error}"
        );
        return;
    }
    // Root bypasses the owner-denying mode used to trigger the acquisition failure.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "live::tools::dump::tests::unacquired_created_entry_is_reported_without_deleting_it",
            "--nocapture",
        ])
        .env(CHILD, root.path())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn relative_destination_and_platform_aliases_remain_usable() {
    let root = tempfile::tempdir_in(".").unwrap();
    let out = root.path().join("dump");
    let mut staging = DumpStaging::new(&out).unwrap();
    staging
        .payload
        .as_ref()
        .unwrap()
        .write_new_file("state.json", b"{}")
        .unwrap();
    staging.publish().unwrap();
    staging.cleanup().unwrap();
    assert_eq!(std::fs::read(out.join("state.json")).unwrap(), b"{}");
}

#[test]
fn dot_and_parent_directory_destinations_are_not_reinterpreted() {
    let root = tempfile::tempdir().unwrap();
    for suffix in [".", "..", "missing/.", "missing/.."] {
        assert!(DumpStaging::new(&root.path().join(suffix)).is_err());
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn replaced_parent_cannot_redirect_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("parent");
    std::fs::create_dir(&parent).unwrap();
    let mut staging = DumpStaging::new(&parent.join("dump")).unwrap();
    std::fs::write(staging.path().join("ours.bin"), b"ours").unwrap();
    let moved = root.path().join("moved");
    std::fs::rename(&parent, &moved).unwrap();
    let unrelated = staging.path().to_owned();
    std::fs::create_dir_all(&unrelated).unwrap();
    std::fs::write(unrelated.join("keep"), b"KEEP").unwrap();
    assert!(staging.check_binding().is_err());
    assert!(staging.publish().is_err());
    staging.cleanup().unwrap();
    assert_eq!(std::fs::read(unrelated.join("keep")).unwrap(), b"KEEP");
    assert_eq!(std::fs::read_dir(moved).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn replaced_container_or_payload_never_deletes_replacement_contents() {
    for replace_container in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let mut staging = DumpStaging::new(&root.path().join("dump")).unwrap();
        std::fs::write(staging.path().join("ours.bin"), b"ours").unwrap();
        let original = if replace_container {
            staging.container_path.clone()
        } else {
            staging.path().to_owned()
        };
        let moved = original.with_extension("moved");
        std::fs::rename(&original, &moved).unwrap();
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("keep"), b"KEEP").unwrap();
        assert!(staging.publish().is_err());
        assert!(staging.cleanup().is_err());
        drop(staging);
        assert_eq!(std::fs::read(original.join("keep")).unwrap(), b"KEEP");
    }
}

#[cfg(unix)]
#[test]
fn cleanup_unlinks_injected_symlinks_without_following_them() {
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"KEEP").unwrap();
    let mut staging = DumpStaging::new(&root.path().join("dump")).unwrap();
    std::os::unix::fs::symlink(&outside, staging.path().join("injected")).unwrap();
    std::os::unix::fs::symlink(outside.join("keep"), staging.path().join("state.json")).unwrap();
    assert!(staging
        .payload
        .as_ref()
        .unwrap()
        .write_new_file("state.json", b"new")
        .is_err());
    staging.cleanup().unwrap();
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"KEEP");
}

#[cfg(unix)]
#[test]
fn symlink_and_fifo_destinations_fail_without_adapter_calls() {
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::tempdir().unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(root.path().join("missing"), &link).unwrap();
    let fifo = root.path().join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for path in [&link, &fifo] {
        let mut adapter = FakeLink::ok(json!({}));
        assert!(dump_memory(&mut adapter, path.to_str().unwrap()).is_err());
        assert!(adapter.last_method.is_none());
        assert!(std::fs::symlink_metadata(path).is_ok());
    }
}

#[cfg(windows)]
#[test]
fn windows_retained_directories_prevent_namespace_replacement() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("parent");
    std::fs::create_dir(&parent).unwrap();
    let mut staging = DumpStaging::new(&parent.join("dump")).unwrap();
    for path in [&parent, &staging.container_path, staging.path()] {
        assert!(std::fs::rename(path, path.with_extension("moved")).is_err());
    }
    staging.cleanup().unwrap();
}

struct RacingLink {
    caps: Capabilities,
    destination: PathBuf,
}
impl EmulatorLink for RacingLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        match method {
            "dump_memory" => {
                std::fs::write(
                    Path::new(params["path"].as_str().unwrap()).join("main.bin"),
                    b"new",
                )
                .unwrap();
                std::fs::create_dir(&self.destination).unwrap();
                std::fs::write(self.destination.join("keep"), b"KEEP").unwrap();
                Ok(json!({}))
            }
            "get_state" => Ok(json!({"state": {"pc": 12}})),
            _ => panic!("unexpected operation"),
        }
    }
}

#[test]
fn destination_appearing_during_adapter_call_survives_failed_publication() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("dump");
    let mut link = RacingLink {
        caps: Capabilities::empty(),
        destination: out.clone(),
    };
    assert!(dump_memory(&mut link, out.to_str().unwrap()).is_err());
    assert_eq!(std::fs::read(out.join("keep")).unwrap(), b"KEEP");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
