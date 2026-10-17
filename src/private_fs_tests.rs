use super::*;

#[cfg(unix)]
#[test]
fn creates_and_reuses_only_owner_private_objects() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD: &str = "EMUCAP_TEST_PRIVATE_FS_UMASK";
    if std::env::var_os(CHILD).is_none() {
        for mode in ["000", "022"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "private_fs::tests::creates_and_reuses_only_owner_private_objects",
                    "--nocapture",
                ])
                .env(CHILD, mode)
                .status()
                .unwrap();
            assert!(
                status.success(),
                "private creation failed under umask {mode}"
            );
        }
        return;
    }
    let requested = std::env::var(CHILD).unwrap();
    let mask = u32::from_str_radix(&requested, 8).unwrap() as libc::mode_t;
    unsafe {
        libc::umask(mask);
    }

    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("managed");
    let nested = root.join("a/b");
    ensure_private_dir_tree(&root, &nested).unwrap();
    for path in [&root, &root.join("a"), &nested] {
        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
    }
    let file = nested.join("state.bin");
    create_private_file(&file).unwrap();
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o077, 0);
    verify_private_file(&file).unwrap();

    let append = nested.join("emulator.log");
    let mut first = open_private_append(&append).unwrap();
    let mut second = open_private_append(&append).unwrap();
    first.write_all(b"one\n").unwrap();
    second.write_all(b"two\n").unwrap();
    drop((first, second));
    assert_eq!(fs::read(&append).unwrap(), b"one\ntwo\n");
    assert_eq!(
        fs::metadata(&append).unwrap().permissions().mode() & 0o077,
        0
    );
}

#[cfg(unix)]
#[test]
fn rejects_unsafe_reused_objects() {
    use std::os::unix::fs::PermissionsExt;

    let outer = tempfile::tempdir().unwrap();
    let directory = outer.path().join("managed");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        ensure_private_dir(&directory).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );

    let file = outer.path().join("open.txt");
    fs::write(&file, b"not private").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        open_private_append(&file).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(fs::read(&file).unwrap(), b"not private");
}

#[cfg(unix)]
#[test]
fn atomic_private_write_refuses_unsafe_existing_target() {
    use std::os::unix::fs::PermissionsExt;

    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("managed");
    create_private_dir(&root).unwrap();
    let target = root.join("config.ini");
    fs::write(&target, b"old").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        write_private_file(&target, b"new").unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(fs::read(&target).unwrap(), b"old");
}

#[cfg(windows)]
#[test]
fn windows_creation_and_append_use_the_private_descriptor() {
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("managed");
    create_private_dir(&root).unwrap();
    verify_private_dir(&root).unwrap();

    let file = root.join("state.bin");
    write_private_file(&file, b"state").unwrap();
    verify_private_file(&file).unwrap();

    let log = root.join("emulator.log");
    let mut first = open_private_append(&log).unwrap();
    let mut second = open_private_append(&log).unwrap();
    first.write_all(b"one\n").unwrap();
    second.write_all(b"two\n").unwrap();
    drop((first, second));
    verify_private_file(&log).unwrap();
    assert_eq!(fs::read(log).unwrap(), b"one\ntwo\n");
}
