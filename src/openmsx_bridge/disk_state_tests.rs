use super::*;

fn fixture() -> (tempfile::TempDir, PreparedSession) {
    let temp = tempfile::tempdir().unwrap();
    let user_data = temp.path().join("generation/user");
    fs::create_dir_all(&user_data).unwrap();
    let session = PreparedSession {
        system: "msx2".into(),
        machine: "Philips_NMS_8250".into(),
        machine_type: "MSX2".into(),
        user_data,
        firmware_manifest_sha256: Some("firmware".into()),
        firmware: vec![],
        media: crate::launch::openmsx::PreparedMedia {
            kind: crate::launch::openmsx::MediaKind::Disk,
            source_path: temp.path().join("source.dsk"),
            source_sha1: "source".into(),
            source_size: 512,
            mounted_path: temp.path().join("generation/disk.dsk"),
            mounted_sha1: "source".into(),
            source_writable: true,
        },
    };
    (temp, session)
}

#[test]
fn snapshot_restores_saved_disk_bytes_without_old_generation_or_source() {
    let (temp, mut session) = fixture();
    let archive = temp.path().join("snapshot.state");
    {
        let scratch = StateScratch::new(temp.path()).unwrap();
        fs::write(scratch.machine(), b"native-state").unwrap();
        fs::write(scratch.disk(), [0x55; 1024]).unwrap();
        publish(&archive, &session, &scratch).unwrap();
    }
    fs::remove_dir_all(session.user_data.parent().unwrap()).unwrap();
    session.user_data = temp.path().join("new-generation/user");
    fs::create_dir_all(&session.user_data).unwrap();
    let restored = prepare(&archive, &session, temp.path()).unwrap();
    assert_eq!(fs::read(restored.disk()).unwrap(), [0x55; 1024]);
    assert_eq!(fs::read(restored.machine()).unwrap(), b"native-state");
    assert!(restored.disk().starts_with(temp.path().join("state")));
    let directory = restored.root.clone();
    drop(restored);
    assert!(!directory.exists());
}

#[test]
fn incompatible_and_corrupt_states_are_rejected_before_extraction() {
    let (temp, session) = fixture();
    let archive = temp.path().join("snapshot.state");
    let scratch = StateScratch::new(temp.path()).unwrap();
    fs::write(scratch.machine(), b"native-state").unwrap();
    fs::write(scratch.disk(), [0x55; 512]).unwrap();
    publish(&archive, &session, &scratch).unwrap();
    for change in 0..4 {
        let mut incompatible = session.clone();
        match change {
            0 => incompatible.media.source_sha1 = "changed".into(),
            1 => incompatible.machine = "other".into(),
            2 => incompatible.firmware_manifest_sha256 = None,
            _ => incompatible.media.source_size += 512,
        }
        assert!(prepare(&archive, &incompatible, temp.path()).is_err());
    }
    let original = fs::read(&archive).unwrap();
    let mut corrupted = original.clone();
    let position = corrupted
        .windows(12)
        .position(|b| b == b"native-state")
        .unwrap();
    corrupted[position] ^= 1;
    fs::write(&archive, corrupted).unwrap();
    assert!(prepare(&archive, &session, temp.path()).is_err());
    fs::write(&archive, &original[..original.len() / 2]).unwrap();
    assert!(prepare(&archive, &session, temp.path()).is_err());
    fs::write(&archive, b"legacy native state").unwrap();
    assert!(prepare(&archive, &session, temp.path())
        .err()
        .unwrap()
        .to_string()
        .contains("legacy/raw"));
}

#[test]
fn oversized_members_and_invalid_sector_images_are_rejected() {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file("manifest.json", SimpleFileOptions::default())
        .unwrap();
    writer.write_all(&vec![0; 16385]).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
    assert!(member(&mut archive, "manifest.json", 16384).is_err());
    assert!(validate_disk(&[0; 513]).is_err());
    assert!(validate_disk(&[]).is_err());
}

#[test]
fn legacy_candidate_uses_only_verified_admitted_source_bytes() {
    let (temp, mut session) = fixture();
    let disk = [0x55; 512];
    fs::write(&session.media.source_path, disk).unwrap();
    session.media.source_sha1 = hex::encode(sha1::Sha1::digest(disk));
    let legacy = temp.path().join("legacy.state");
    fs::write(&legacy, b"<?xml native state fixture").unwrap();
    let scratch = prepare(&legacy, &session, temp.path()).unwrap();
    assert_eq!(fs::read(scratch.disk()).unwrap(), disk);
    assert_ne!(scratch.disk(), session.media.source_path);
    // Admission identity is immutable even if the source path is overwritten.
    fs::write(&session.media.source_path, [0xaa; 512]).unwrap();
    assert!(prepare(&legacy, &session, temp.path()).is_err());
    fs::remove_file(&session.media.source_path).unwrap();
    assert!(prepare(&legacy, &session, temp.path()).is_err());
}

#[test]
fn scratch_uses_a_shallow_runtime_owned_directory() {
    let (temp, _session) = fixture();
    let scratch = StateScratch::new(temp.path()).unwrap();
    assert!(scratch.machine().starts_with(temp.path().join("state")));
    assert_eq!(scratch.machine().file_name().unwrap(), "machine.state");
    assert_eq!(scratch.disk().file_name().unwrap(), "disk.dsk");
    assert_eq!(scratch.root.parent().unwrap(), temp.path().join("state"));
}

#[cfg(windows)]
#[test]
fn windows_native_path_guard_uses_utf16_units_and_rejects_260() {
    let accepted = PathBuf::from(format!("C:\\{}", "a".repeat(256)));
    let rejected = PathBuf::from(format!("C:\\{}", "a".repeat(257)));
    assert!(require_native_path(&accepted).is_ok());
    assert!(require_native_path(&rejected).is_err());
}
