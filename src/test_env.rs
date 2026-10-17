use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Serialize tests that read or mutate the process environment.
pub(crate) fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Restore the selected environment variables when the test scope ends.
pub(crate) struct EnvGuard(Vec<(&'static str, Option<OsString>)>);

impl EnvGuard {
    pub(crate) fn new(keys: &[&'static str]) -> Self {
        Self(
            keys.iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect(),
        )
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// Temporary directory whose exposed root is created with emucap's private-filesystem policy.
pub(crate) struct PrivateTempDir {
    _outer: tempfile::TempDir,
    path: PathBuf,
}

impl PrivateTempDir {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn private_tempdir() -> io::Result<PrivateTempDir> {
    let outer = tempfile::tempdir()?;
    let path = outer.path().join("managed");
    crate::private_fs::create_private_dir(&path)?;
    Ok(PrivateTempDir {
        _outer: outer,
        path,
    })
}
