use super::sha1_of_file;
use std::io::Write;

#[test]
fn hashes_known_content() {
    // "abc"의 SHA-1은 a9993e364706816aba3e25717850c26c9cd0d89d
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"abc").unwrap();
    let hash = sha1_of_file(f.path()).unwrap();
    assert_eq!(hash, "a9993e364706816aba3e25717850c26c9cd0d89d");
}

#[test]
fn errors_on_missing_file() {
    let p = std::path::Path::new("/nonexistent/does-not-exist.sfc");
    assert!(sha1_of_file(p).is_err());
}

#[test]
fn rejects_oversized_file_before_hashing_and_accepts_exact_limit() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"abc").unwrap();
    assert!(super::sha1_of_bounded_file(file.path(), 2).is_err());
    assert_eq!(
        super::sha1_of_bounded_file(file.path(), 3).unwrap(),
        sha1_of_file(file.path()).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlink_and_fifo_without_opening_a_stream() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("rom");
    std::fs::write(&file, b"abc").unwrap();
    let link = dir.path().join("link");
    symlink(&file, &link).unwrap();
    assert!(sha1_of_file(&link).is_err());
    let fifo = dir.path().join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(sha1_of_file(&fifo).is_err());
}
