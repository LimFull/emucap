use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path};

#[cfg(windows)]
#[path = "private_fs/windows.rs"]
pub(crate) mod windows;

#[cfg(unix)]
const PRIVATE_DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(unix)]
fn verify_unix_owner_and_mode(metadata: &fs::Metadata, path: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let current = unsafe { libc::geteuid() };
    if metadata.uid() != current {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "private path is owned by uid {}, current uid is {current}: {}",
                metadata.uid(),
                path.display()
            ),
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "private path is accessible by group or other users: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn open_directory_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

pub(crate) fn verify_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let file = open_directory_no_follow(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() {
            return Err(invalid(format!(
                "private path is not a directory: {}",
                path.display()
            )));
        }
        verify_unix_owner_and_mode(&metadata, path)
    }
    #[cfg(windows)]
    {
        windows::verify_private_dir(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid(format!(
                "private path is not a real directory: {}",
                path.display()
            )));
        }
        Ok(())
    }
}

pub(crate) fn verify_private_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid(format!(
                "private path is not a regular file: {}",
                path.display()
            )));
        }
        verify_unix_owner_and_mode(&metadata, path)
    }
    #[cfg(windows)]
    {
        windows::verify_private_file(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(invalid(format!(
                "private path is not a regular file: {}",
                path.display()
            )));
        }
        Ok(())
    }
}

pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = fs::DirBuilder::new();
        builder.mode(PRIVATE_DIR_MODE);
        builder.create(path)?;
        if let Err(error) = verify_private_dir(path) {
            let _ = fs::remove_dir(path);
            return Err(error);
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        windows::create_private_dir(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        fs::create_dir(path)
    }
}

pub(crate) fn ensure_private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => verify_private_dir(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_private_dir(path),
        Err(error) => Err(error),
    }
}

/// Create or verify `root` and every normal component through `target`.
/// `root` is the first emucap-owned path; its existing parent remains external.
pub(crate) fn ensure_private_dir_tree(root: &Path, target: &Path) -> io::Result<()> {
    if !root.is_absolute() || !target.starts_with(root) {
        return Err(invalid("private directory tree escapes its absolute root"));
    }
    let parent = root
        .parent()
        .ok_or_else(|| invalid("private directory root has no parent"))?;
    fs::create_dir_all(parent)?;
    ensure_private_dir(root)?;

    let relative = target
        .strip_prefix(root)
        .map_err(|_| invalid("private directory target escapes its root"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(invalid(
                "private directory target contains a non-normal component",
            ));
        };
        current.push(component);
        ensure_private_dir(&current)?;
    }
    Ok(())
}

pub(crate) fn create_private_file(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        if let Err(error) = verify_unix_owner_and_mode(&file.metadata()?, path) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
        Ok(file)
    }
    #[cfg(windows)]
    {
        windows::create_private_file(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
    }
}

pub(crate) fn open_private_append(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let file = fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid(format!(
                "private append path is not a regular file: {}",
                path.display()
            )));
        }
        verify_unix_owner_and_mode(&metadata, path)?;
        Ok(file)
    }
    #[cfg(windows)]
    {
        windows::open_private_append(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        fs::OpenOptions::new().append(true).create(true).open(path)
    }
}

pub(crate) fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("private file has no parent"))?;
    match fs::symlink_metadata(path) {
        Ok(_) => verify_private_file(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if path.file_name().is_none() {
        return Err(invalid("private file has no filename"));
    }
    let temp = parent.join(format!(
        ".emucap-private-{}",
        ulid::Ulid::generate().to_string().to_ascii_lowercase()
    ));
    let result = (|| {
        let mut file = create_private_file(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        crate::path_safety::replace_file_atomically(&temp, path)?;
        verify_private_file(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Convert an existing current-user object created inside a private managed directory into the
/// explicit private form before Rust consumes or publishes it.
pub(crate) fn privatize_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        let current = unsafe { libc::geteuid() };
        if !metadata.is_file() || metadata.uid() != current {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("refusing to privatize unowned file: {}", path.display()),
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
        verify_private_file(path)
    }
    #[cfg(windows)]
    {
        windows::privatize_file(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        verify_private_file(path)
    }
}

pub(crate) fn privatize_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let file = open_directory_no_follow(path)?;
        let metadata = file.metadata()?;
        let current = unsafe { libc::geteuid() };
        if !metadata.is_dir() || metadata.uid() != current {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing to privatize unowned directory: {}",
                    path.display()
                ),
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(PRIVATE_DIR_MODE))?;
        verify_private_dir(path)
    }
    #[cfg(windows)]
    {
        windows::privatize_dir(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        verify_private_dir(path)
    }
}

#[cfg(test)]
#[path = "private_fs_tests.rs"]
mod tests;
