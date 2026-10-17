use super::dump::*;
use std::fs;

#[test]
fn load_reads_regions_and_bins() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("wram.bin"), [1u8, 2, 3]).unwrap();
    fs::write(tmp.path().join("cram.bin"), [9u8, 9]).unwrap();
    let meta = r#"[
      { "name": "wram", "memory_type": "snesWorkRam", "base_address": 8257536, "size": 3 },
      { "name": "cram", "memory_type": "snesCgRam", "base_address": 0, "size": 2 }
    ]"#;
    fs::write(tmp.path().join("regions.json"), meta).unwrap();

    let set = load(tmp.path()).unwrap();
    assert_eq!(set.regions.get("wram").unwrap().bytes, vec![1, 2, 3]);
    assert_eq!(set.regions.get("wram").unwrap().base_address, 8257536);
    assert_eq!(set.regions.get("cram").unwrap().bytes, vec![9, 9]);
}

#[test]
fn load_errors_when_meta_missing() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(load(tmp.path()), Err(DumpError::MetaNotFound(_))));
}

#[test]
fn load_errors_when_bin_missing() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("regions.json"),
        r#"[{ "name": "wram", "memory_type": "snesWorkRam", "base_address": 0, "size": 1 }]"#,
    )
    .unwrap();
    assert!(matches!(load(tmp.path()), Err(DumpError::BinNotFound(_))));
}

#[test]
fn load_rejects_a_region_name_that_becomes_a_path() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("regions.json"),
        r#"[{ "name": "../outside", "memory_type": "ram", "base_address": 0, "size": 1 }]"#,
    )
    .unwrap();
    assert!(matches!(load(tmp.path()), Err(DumpError::UnsafeMember(_))));
}

#[cfg(unix)]
#[test]
fn load_rejects_a_symlink_region_member() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside.bin");
    fs::write(&outside, [1u8]).unwrap();
    symlink(&outside, tmp.path().join("wram.bin")).unwrap();
    fs::write(
        tmp.path().join("regions.json"),
        r#"[{ "name": "wram", "memory_type": "ram", "base_address": 0, "size": 1 }]"#,
    )
    .unwrap();
    assert!(matches!(load(tmp.path()), Err(DumpError::UnsafeMember(_))));
}

#[test]
fn declared_region_size_is_enforced_on_the_read_handle() {
    for (declared, bytes, accepted) in [
        (0, vec![], true),
        (1, vec![1], true),
        (0, vec![1], false),
        (2, vec![1], false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ram.bin"), bytes).unwrap();
        fs::write(
            dir.path().join("regions.json"),
            serde_json::to_vec(&vec![RegionMeta {
                name: "ram".into(),
                memory_type: "ram".into(),
                base_address: 0,
                size: declared,
            }])
            .unwrap(),
        )
        .unwrap();
        let result = load(dir.path());
        if accepted {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(DumpError::SizeMismatch(_))));
        }
    }
}

#[test]
fn aggregate_dump_admission_precedes_member_reads() {
    let dir = tempfile::tempdir().unwrap();
    let entries: Vec<_> = (0..2)
        .map(|index| RegionMeta {
            name: format!("ram{index}"),
            memory_type: "ram".into(),
            base_address: 0,
            size: MAX_DUMP_BYTES / 2 + 1,
        })
        .collect();
    fs::write(
        dir.path().join("regions.json"),
        serde_json::to_vec(&entries).unwrap(),
    )
    .unwrap();
    assert!(matches!(load(dir.path()), Err(DumpError::SizeMismatch(_))));
}
