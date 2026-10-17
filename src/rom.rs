use sha1::{Digest, Sha1};
use std::io::Read;
use std::path::Path;

pub fn sha1_of_file(path: &Path) -> std::io::Result<String> {
    sha1_of_bounded_file(path, MAX_IDENTITY_FILE_BYTES)
}

// Covers single-file disc images as well as ROMs and state artifacts. Hashing is
// streaming; this is an admission limit, not an allocation or elapsed-time limit.
pub const MAX_IDENTITY_FILE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

fn sha1_of_bounded_file(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    let mut file = crate::path_safety::open_regular_file_no_follow(path)?;
    let before = file.metadata()?;
    let size = before.len();
    if size > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "identity file exceeds byte limit",
        ));
    }
    let mut limited = Read::by_ref(&mut file).take(size + 1);
    let mut bytes_read = 0_u64;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = limited.read(&mut buf)?;
        if n == 0 {
            break;
        }
        bytes_read += n as u64;
        hasher.update(&buf[..n]);
    }
    let after = file.metadata()?;
    if bytes_read != size || after.len() != size || after.modified().ok() != before.modified().ok()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "identity file changed while hashing",
        ));
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
#[path = "rom_tests.rs"]
mod rom_tests;
