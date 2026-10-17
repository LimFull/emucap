use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

/// Portable identifier grammar for values that become file or directory names.
///
/// Identifiers contain one or more non-empty ASCII-alphanumeric segments joined
/// by single hyphens. This deliberately excludes punctuation that has path or
/// shell meaning on any supported platform.
pub fn is_hyphenated_ascii_id(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.split('-').all(|segment| {
            !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        && !is_windows_reserved_component(value)
}

/// A relative member path whose components cannot change traversal roots.
/// Backslashes are rejected even on Unix so a record remains safe when moved to
/// Windows.
pub fn is_portable_relative_member(value: &str) -> bool {
    if value.is_empty() || value.len() > 4096 || value.contains('\\') {
        return false;
    }
    let path = Path::new(value);
    !path.is_absolute()
        && path.components().all(|component| match component {
            Component::Normal(component) => component.to_str().is_some_and(is_portable_component),
            _ => false,
        })
}

fn is_portable_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.ends_with(['.', ' '])
        && !value
            .chars()
            .any(|character| character <= '\u{1f}' || r#"<>:"/\\|?*"#.contains(character))
        && !is_windows_reserved_component(value)
}

fn is_windows_reserved_component(value: &str) -> bool {
    let stem = value.split('.').next().unwrap_or(value);
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) || upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
        .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
}

/// A portable leaf file name. Dots are allowed inside the name for file
/// extensions, but hidden, current-directory, and parent-directory forms are
/// rejected.
pub fn is_portable_file_name(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && !value.starts_with('.')
        && !value.ends_with('.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && is_portable_relative_member(value)
        && Path::new(value)
            .parent()
            .is_some_and(|parent| parent.as_os_str().is_empty())
}

/// Resolve an existing regular member without accepting symlinks in any
/// bundle-owned component.
pub fn regular_member_path(root: &Path, relative: &str) -> io::Result<PathBuf> {
    if !is_portable_relative_member(relative) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "member path must remain relative to its managed root",
        ));
    }
    let root_metadata = fs::symlink_metadata(root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed root is not a real directory",
        ));
    }
    let canonical_root = fs::canonicalize(root)?;
    let components = Path::new(relative).components().collect::<Vec<_>>();
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "member path contains an unsafe component",
            ));
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        let last = index + 1 == components.len();
        if metadata.file_type().is_symlink()
            || (last && !metadata.is_file())
            || (!last && !metadata.is_dir())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "managed member contains a symlink or unexpected file type",
            ));
        }
    }
    let canonical = fs::canonicalize(&current)?;
    if !canonical.starts_with(&canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed member escapes its root",
        ));
    }
    Ok(canonical)
}

/// Open an existing regular file without following a symbolic link in the
/// final path component. Callers that own a containing directory should still
/// validate those directory components separately.
pub fn open_regular_file_no_follow(path: &Path) -> io::Result<fs::File> {
    let path_metadata = fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a regular non-symlink file",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK prevents a replacement FIFO from hanging the caller before fstat can
        // reject it. It has no effect on regular-file reads.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let handle_metadata = file.metadata()?;
    if !handle_metadata.is_file()
        || !opened_file_is_safe_regular(&path_metadata, &file, &handle_metadata)?
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "opened path is not the validated regular file",
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn opened_file_is_safe_regular(
    path_metadata: &fs::Metadata,
    _file: &fs::File,
    handle_metadata: &fs::Metadata,
) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;

    Ok(
        path_metadata.dev() == handle_metadata.dev()
            && path_metadata.ino() == handle_metadata.ino(),
    )
}

#[cfg(windows)]
fn opened_file_is_safe_regular(
    _path_metadata: &fs::Metadata,
    file: &fs::File,
    _handle_metadata: &fs::Metadata,
) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, GetFileType, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_TYPE_DISK,
    };

    let handle = file.as_raw_handle();
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `handle` belongs to the live `File`, and `information` is writable for the call.
    let ok = unsafe { GetFileInformationByHandle(handle, &mut information) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // FILE_FLAG_OPEN_REPARSE_POINT makes the handle describe the final reparse point itself.
    // Reject it explicitly, along with directories and non-disk handles, instead of inferring
    // identity from a replaceable path's length and timestamp.
    let unsafe_attributes = FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT;
    Ok(unsafe { GetFileType(handle) } == FILE_TYPE_DISK
        && information.dwFileAttributes & unsafe_attributes == 0)
}

#[cfg(all(not(unix), not(windows)))]
fn opened_file_is_safe_regular(
    path_metadata: &fs::Metadata,
    _file: &fs::File,
    handle_metadata: &fs::Metadata,
) -> io::Result<bool> {
    Ok(path_metadata.len() == handle_metadata.len()
        && path_metadata.modified().ok() == handle_metadata.modified().ok())
}

/// Open a regular member relative to one retained root handle without following any member-path
/// link or reparse point. Every component is resolved from its acquired parent handle.
pub fn open_regular_member_no_follow(root: &Path, relative: &str) -> io::Result<fs::File> {
    if !is_portable_relative_member(relative) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "member path must remain relative to its managed root",
        ));
    }
    crate::rooted_fs::RootedDirectory::open(root)?.open_regular_member(Path::new(relative))
}

pub fn read_bounded_regular_member(
    root: &Path,
    relative: &str,
    max_bytes: u64,
) -> io::Result<Vec<u8>> {
    let file = open_regular_member_no_follow(root, relative)?;
    read_bounded_opened_regular_file(file, max_bytes)
}

pub fn read_bounded_regular_file_no_follow(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let file = open_regular_file_no_follow(path)?;
    read_bounded_opened_regular_file(file, max_bytes)
}

pub(crate) fn read_bounded_opened_regular_file(
    mut file: fs::File,
    max_bytes: u64,
) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "input is not a regular file",
        ));
    }
    if metadata.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("managed member exceeds {max_bytes} bytes"),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("managed member exceeds {max_bytes} bytes"),
        ));
    }
    Ok(bytes)
}

pub fn read_bounded_utf8_regular_file_no_follow(path: &Path, max_bytes: u64) -> io::Result<String> {
    String::from_utf8(read_bounded_regular_file_no_follow(path, max_bytes)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Replace an explicitly authorized output file atomically. A destination
/// symlink is rejected rather than followed or silently replaced.
pub fn atomic_write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output path has no parent"))?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    fs::create_dir_all(parent)?;
    validate_file_destination(path)?;
    let file_name = output_file_name(path)?;
    let temp = unique_output_sibling(parent, file_name, "tmp");
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file_atomically(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Stream an explicitly authorized source into a sibling temporary file and
/// publish it atomically. The source path may be an operator-selected symlink;
/// the managed destination may not be one.
pub fn atomic_copy_file(source: &Path, path: &Path) -> io::Result<u64> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output path has no parent"))?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    fs::create_dir_all(parent)?;
    validate_file_destination(path)?;
    let file_name = output_file_name(path)?;
    let temp = unique_output_sibling(parent, file_name, "copy");
    let result = (|| {
        let mut input = fs::File::open(source)?;
        if !input.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "copy source is not a regular file",
            ));
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut output = options.open(&temp)?;
        let copied = io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        drop(output);
        if let Ok(permissions) = input.metadata().map(|metadata| metadata.permissions()) {
            fs::set_permissions(&temp, permissions)?;
        }
        replace_file_atomically(&temp, path)?;
        Ok(copied)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn validate_file_destination(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output path is a symbolic link",
        )),
        Ok(metadata) if metadata.is_dir() => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output path is a directory",
        )),
        Ok(metadata) if !metadata.is_file() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output path is not a regular file",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn output_file_name(path: &Path) -> io::Result<&str> {
    path.file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid output file name"))
}

fn unique_output_sibling(parent: &Path, file_name: &str, label: &str) -> PathBuf {
    parent.join(format!(
        ".{file_name}.{label}-{}",
        ulid::Ulid::generate().to_string().to_ascii_lowercase()
    ))
}

/// Flush directory entries on Unix. Windows publication uses a write-through move
/// after flushing every member file; opening a directory as a regular file is invalid.
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

pub(crate) fn rename_directory(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        move_path_windows(source, destination, false)
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, destination)
    }
}

#[cfg(not(windows))]
pub(crate) fn replace_file_atomically(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
pub(crate) fn replace_file_atomically(source: &Path, destination: &Path) -> io::Result<()> {
    move_path_windows(source, destination, true)
}

#[cfg(windows)]
fn move_path_windows(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    // std::fs handles long Windows paths internally; raw Win32 calls need the
    // verbatim parent explicitly. Resolve the parent so a new destination and
    // the source's final component keep their existing rename semantics.
    let source = windows_move_path(source)?;
    let destination = windows_move_path(destination)?;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both buffers are NUL-terminated and remain alive for the duration of the call.
    let ok = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH
                | if replace {
                    MOVEFILE_REPLACE_EXISTING
                } else {
                    0
                },
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn windows_move_path(path: &Path) -> io::Result<PathBuf> {
    let path = std::path::absolute(path)?;
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "move path has no final component",
        )
    })?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "move path has no parent"))?;
    Ok(fs::canonicalize(parent)?.join(name))
}
