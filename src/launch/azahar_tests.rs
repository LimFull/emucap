use super::*;
use crate::test_env::{lock_env, EnvGuard};

fn write_build(dir: &Path, patchset: &str, core: &[u8]) -> PathBuf {
    std::fs::write(dir.join("azahar_libretro.dylib"), core).unwrap();
    let info = dir.join("emucap-azahar-build.json");
    std::fs::write(
        &info,
        serde_json::json!({
            "version": lock_value("AZAHAR_VERSION").unwrap(),
            "archive_sha256": lock_value("AZAHAR_ARCHIVE_SHA256").unwrap(),
            "patchset_sha256": patchset,
            "core": "azahar_libretro.dylib",
            "core_sha256": sha256_file(&dir.join("azahar_libretro.dylib")).unwrap(),
        })
        .to_string(),
    )
    .unwrap();
    info
}

#[test]
fn core_admission_requires_the_locked_patch_stack_and_core_digest() {
    let _lock = lock_env();
    let _env = EnvGuard::new(&["EMUCAP_AZAHAR_BUILD_INFO"]);
    let dir = tempfile::tempdir().unwrap();
    let locked = lock_value("AZAHAR_PATCHSET_SHA256").unwrap();
    let info = write_build(dir.path(), locked, b"core");
    std::env::set_var("EMUCAP_AZAHAR_BUILD_INFO", &info);
    let admitted = require_compatible_core(Path::new("/nonexistent")).unwrap();
    assert_eq!(admitted.path, dir.path().join("azahar_libretro.dylib"));

    std::fs::write(dir.path().join("azahar_libretro.dylib"), b"replaced").unwrap();
    assert!(require_compatible_core(Path::new("/nonexistent")).is_err());

    write_build(dir.path(), &"0".repeat(64), b"core");
    let error = require_compatible_core(Path::new("/nonexistent")).unwrap_err();
    assert!(
        error.to_string().contains("AZAHAR_PATCHSET_SHA256"),
        "{error}"
    );
}

#[test]
fn only_directly_loadable_titles_are_accepted() {
    for (name, ok) in [
        ("a.3ds", true),
        ("a.CXI", true),
        ("a.3dsx", true),
        ("a.cia", false),
        ("a.zip", false),
    ] {
        assert_eq!(accepts_content_path(Path::new(name)), ok, "{name}");
    }
}
