//! Stable handle-relative access beneath an admitted directory.
//!
//! Path strings select a root once. Descendants are then resolved from retained directory handles,
//! so replacing an ancestor after admission cannot redirect a member read.
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};

pub(crate) struct RootedDirectory {
    platform: platform::Directory,
    canonical: PathBuf,
}

impl RootedDirectory {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let canonical = fs::canonicalize(path)?;
        Ok(Self {
            platform: platform::Directory::open(&canonical)?,
            canonical,
        })
    }

    pub(crate) fn open_regular_member(&self, relative: &Path) -> io::Result<File> {
        let components = relative.components().collect::<Vec<_>>();
        if components.is_empty()
            || components
                .iter()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "member path must contain only normal relative components",
            ));
        }
        self.platform.open_regular_member(&components)
    }

    pub(crate) fn read_bounded_regular_member(
        &self,
        relative: &Path,
        max_bytes: u64,
    ) -> io::Result<Vec<u8>> {
        let file = self.open_regular_member(relative)?;
        crate::path_safety::read_bounded_opened_regular_file(file, max_bytes)
    }

    fn inspect_output_leaf(&self, leaf: &OsStr) -> io::Result<()> {
        self.platform.inspect_output_leaf(leaf)
    }

    fn write_replace(&self, leaf: &OsStr, bytes: &[u8]) -> io::Result<()> {
        self.platform.write_replace(leaf, bytes)
    }
}

pub(crate) struct OutputSlot {
    parent: RootedDirectory,
    leaf: OsString,
}

impl OutputSlot {
    pub(crate) fn admit(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output path must be absolute",
            ));
        }
        let parent_path = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing output parent"))?;
        let leaf = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing output name"))?
            .to_owned();
        let leaf_text = leaf.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "output name must be UTF-8")
        })?;
        if !crate::path_safety::is_portable_relative_member(leaf_text)
            || Path::new(leaf_text)
                .parent()
                .is_none_or(|parent| !parent.as_os_str().is_empty())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output name must be one portable path component",
            ));
        }
        let parent = RootedDirectory::open(parent_path)?;
        parent.inspect_output_leaf(&leaf)?;
        Ok(Self { parent, leaf })
    }

    pub(crate) fn reject_parent_within(&self, root: &Path) -> io::Result<()> {
        let root = fs::canonicalize(root)?;
        if self.parent.canonical.starts_with(root) {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "output parent is inside protected managed storage",
            ))
        } else {
            Ok(())
        }
    }

    pub(crate) fn publish(&self, bytes: &[u8]) -> io::Result<()> {
        self.parent.write_replace(&self.leaf, bytes)
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::ffi::CString;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    pub(super) struct Directory {
        file: OwnedFd,
    }

    fn component_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
    }

    fn openat_owned(parent: &OwnedFd, name: &OsStr, flags: libc::c_int) -> io::Result<OwnedFd> {
        let name = component_name(name)?;
        // SAFETY: the directory descriptor and NUL-terminated component remain live for the call.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: `openat` returned a new descriptor owned by this call.
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }
    }

    fn createat_owned(parent: &OwnedFd, name: &OsStr) -> io::Result<OwnedFd> {
        let name = component_name(name)?;
        // SAFETY: the parent and name remain live; mode is supplied because O_CREAT is set.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: `openat` returned a new descriptor owned by this call.
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }
    }

    impl Directory {
        pub(super) fn open(path: &Path) -> io::Result<Self> {
            if !path.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "rooted directory must be absolute",
                ));
            }
            let root = CString::new("/").expect("root path has no NUL");
            let flags = libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | libc::O_CLOEXEC;
            // SAFETY: `root` is a valid NUL-terminated path and a successful descriptor is owned.
            let fd = unsafe { libc::open(root.as_ptr(), flags) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `fd` was returned by `open` and is exclusively owned here.
            let mut current = unsafe { OwnedFd::from_raw_fd(fd) };
            for component in path.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(name) => current = openat_owned(&current, name, flags)?,
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "canonical root contains an invalid component",
                        ))
                    }
                }
            }
            Ok(Self { file: current })
        }

        pub(super) fn open_regular_member(&self, components: &[Component<'_>]) -> io::Result<File> {
            let directory_flags = libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | libc::O_CLOEXEC;
            let mut directory = openat_owned(&self.file, OsStr::new("."), directory_flags)?;
            for component in &components[..components.len() - 1] {
                let Component::Normal(name) = component else {
                    unreachable!("components were admitted by RootedDirectory")
                };
                directory = openat_owned(&directory, name, directory_flags)?;
            }
            let Component::Normal(name) = components[components.len() - 1] else {
                unreachable!("components were admitted by RootedDirectory")
            };
            let file = openat_owned(
                &directory,
                name,
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )?;
            let file = File::from(file);
            if !file.metadata()?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "rooted member is not a regular file",
                ));
            }
            Ok(file)
        }

        pub(super) fn inspect_output_leaf(&self, leaf: &OsStr) -> io::Result<()> {
            let leaf = component_name(leaf)?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: descriptor and name are live; stat points to writable storage.
            let result = unsafe {
                libc::fstatat(
                    self.file.as_raw_fd(),
                    leaf.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result < 0 {
                let error = io::Error::last_os_error();
                return if error.kind() == io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                };
            }
            // SAFETY: successful fstatat initialized the structure.
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT == libc::S_IFREG {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "output destination is not a regular non-link file",
                ))
            }
        }

        pub(super) fn write_replace(&self, leaf: &OsStr, bytes: &[u8]) -> io::Result<()> {
            let temporary = OsString::from(format!(
                ".emucap-output-{}.tmp",
                ulid::Ulid::generate().to_string().to_ascii_lowercase()
            ));
            let temporary_name = component_name(&temporary)?;
            let leaf_name = component_name(leaf)?;
            let descriptor = createat_owned(&self.file, &temporary)?;
            let mut owned = true;
            let result = (|| {
                let mut file = File::from(descriptor);
                file.write_all(bytes)?;
                file.sync_all()?;
                let fd = file.into_raw_fd();
                // SAFETY: ownership was transferred from File; close reports delayed write errors.
                if unsafe { libc::close(fd) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                self.inspect_output_leaf(leaf)?;
                // SAFETY: both names are relative to the same retained directory descriptor.
                if unsafe {
                    libc::renameat(
                        self.file.as_raw_fd(),
                        temporary_name.as_ptr(),
                        self.file.as_raw_fd(),
                        leaf_name.as_ptr(),
                    )
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                owned = false;
                // SAFETY: fsync on a live directory descriptor persists the renamed entry.
                if unsafe { libc::fsync(self.file.as_raw_fd()) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            })();
            if owned {
                // SAFETY: unlinkat removes only the exclusively created temporary entry.
                unsafe {
                    libc::unlinkat(self.file.as_raw_fd(), temporary_name.as_ptr(), 0);
                }
            }
            result
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::io::Write;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FileRenameInformation, NtCreateFile, NtSetInformationFile, FILE_CREATE,
        FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
        FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT,
    };
    use windows_sys::Win32::Foundation::{
        RtlNtStatusToDosError, OBJ_CASE_INSENSITIVE, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
        FILE_SHARE_WRITE, SYNCHRONIZE,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    pub(super) struct Directory {
        file: File,
        _ancestors: Vec<File>,
    }

    fn open_path(path: &Path) -> io::Result<File> {
        fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    fn require_real_directory(file: &File) -> io::Result<()> {
        let metadata = file.metadata()?;
        if metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rooted path component is not a real directory",
            ))
        }
    }

    fn relative_open(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
        let mut encoded = name.encode_wide().collect::<Vec<_>>();
        let length = u16::try_from(encoded.len().saturating_mul(2)).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "rooted member name is too long",
            )
        })?;
        if encoded.is_empty() || encoded.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid rooted member name",
            ));
        }
        let name = UNICODE_STRING {
            Length: length,
            MaximumLength: length,
            Buffer: encoded.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.as_raw_handle(),
            ObjectName: &name,
            Attributes: OBJ_CASE_INSENSITIVE,
            ..Default::default()
        };
        let mut handle = std::ptr::null_mut();
        let mut status = IO_STATUS_BLOCK::default();
        let options = if directory {
            FILE_DIRECTORY_FILE
        } else {
            FILE_NON_DIRECTORY_FILE
        };
        let access = if directory {
            FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY
        } else {
            FILE_GENERIC_READ
        };
        // Excluding FILE_SHARE_DELETE pins directory entries while descendants are acquired.
        let result = unsafe {
            NtCreateFile(
                &mut handle,
                access | SYNCHRONIZE,
                &attributes,
                &mut status,
                std::ptr::null(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                FILE_OPEN,
                options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                std::ptr::null(),
                0,
            )
        };
        if result < 0 {
            return Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(result) } as i32,
            ));
        }
        // SAFETY: `NtCreateFile` returned a new owned kernel handle.
        let file = unsafe { File::from_raw_handle(handle) };
        let metadata = file.metadata()?;
        let attributes = metadata.file_attributes();
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || directory != (attributes & FILE_ATTRIBUTE_DIRECTORY != 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rooted member is a reparse point or has the wrong type",
            ));
        }
        Ok(file)
    }

    fn relative_create_private(parent: &File, name: &OsStr) -> io::Result<File> {
        let mut encoded = name.encode_wide().collect::<Vec<_>>();
        let length = u16::try_from(encoded.len().saturating_mul(2))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "output name is too long"))?;
        let name = UNICODE_STRING {
            Length: length,
            MaximumLength: length,
            Buffer: encoded.as_mut_ptr(),
        };
        let mut security = crate::private_fs::windows::PrivateSecurity::new(false)?;
        let attributes = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.as_raw_handle(),
            ObjectName: &name,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: security
                .descriptor_ptr()
                .cast::<windows_sys::Win32::Security::SECURITY_DESCRIPTOR>()
                .cast_const(),
            ..Default::default()
        };
        let mut handle = std::ptr::null_mut();
        let mut status = IO_STATUS_BLOCK::default();
        let result = unsafe {
            NtCreateFile(
                &mut handle,
                FILE_GENERIC_WRITE | DELETE | SYNCHRONIZE,
                &attributes,
                &mut status,
                std::ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                FILE_CREATE,
                FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                std::ptr::null(),
                0,
            )
        };
        if result < 0 {
            return Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(result) } as i32,
            ));
        }
        // SAFETY: `NtCreateFile` returned a new owned kernel handle.
        let file = unsafe { File::from_raw_handle(handle) };
        crate::private_fs::windows::verify_private_handle(file.as_raw_handle(), false)?;
        Ok(file)
    }

    fn rename_handle(file: &File, parent: &File, destination: &OsStr) -> io::Result<()> {
        let name = destination.encode_wide().collect::<Vec<_>>();
        let bytes = std::mem::size_of::<FILE_RENAME_INFORMATION>() + name.len() * 2;
        let mut buffer = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        // SAFETY: the aligned buffer holds the header and complete UTF-16 destination.
        unsafe {
            (*info).Anonymous.ReplaceIfExists = true;
            (*info).RootDirectory = parent.as_raw_handle();
            (*info).FileNameLength = (name.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
        }
        let mut status = IO_STATUS_BLOCK::default();
        let result = unsafe {
            NtSetInformationFile(
                file.as_raw_handle(),
                &mut status,
                info.cast(),
                bytes as u32,
                FileRenameInformation,
            )
        };
        if result < 0 {
            Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(result) } as i32,
            ))
        } else {
            Ok(())
        }
    }

    impl Directory {
        pub(super) fn open(path: &Path) -> io::Result<Self> {
            let mut handles = Vec::new();
            for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
                let file = open_path(ancestor)?;
                require_real_directory(&file)?;
                handles.push(file);
            }
            let file = handles.pop().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "missing rooted directory")
            })?;
            Ok(Self {
                file,
                _ancestors: handles,
            })
        }

        pub(super) fn open_regular_member(&self, components: &[Component<'_>]) -> io::Result<File> {
            let mut directories = Vec::new();
            let mut current = &self.file;
            for component in &components[..components.len() - 1] {
                let Component::Normal(name) = component else {
                    unreachable!("components were admitted by RootedDirectory")
                };
                directories.push(relative_open(current, name, true)?);
                current = directories.last().expect("a directory was just pushed");
            }
            let Component::Normal(name) = components[components.len() - 1] else {
                unreachable!("components were admitted by RootedDirectory")
            };
            relative_open(current, name, false)
        }

        pub(super) fn inspect_output_leaf(&self, leaf: &OsStr) -> io::Result<()> {
            match relative_open(&self.file, leaf, false) {
                Ok(_) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        }

        pub(super) fn write_replace(&self, leaf: &OsStr, bytes: &[u8]) -> io::Result<()> {
            let temporary = OsString::from(format!(
                ".emucap-output-{}.tmp",
                ulid::Ulid::generate().to_string().to_ascii_lowercase()
            ));
            let mut file = relative_create_private(&self.file, &temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            self.inspect_output_leaf(leaf)?;
            rename_handle(&file, &self.file, leaf)
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;

    pub(super) struct Directory {
        path: std::path::PathBuf,
    }

    impl Directory {
        pub(super) fn open(path: &Path) -> io::Result<Self> {
            if !path.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "rooted path is not a directory",
                ));
            }
            Ok(Self { path: path.into() })
        }

        pub(super) fn open_regular_member(&self, components: &[Component<'_>]) -> io::Result<File> {
            let relative = components
                .iter()
                .fold(std::path::PathBuf::new(), |mut path, part| {
                    path.push(part.as_os_str());
                    path
                });
            crate::path_safety::open_regular_file_no_follow(&self.path.join(relative))
        }

        pub(super) fn inspect_output_leaf(&self, leaf: &OsStr) -> io::Result<()> {
            super::super::validate_file_destination(&self.path.join(leaf))
        }

        pub(super) fn write_replace(&self, leaf: &OsStr, bytes: &[u8]) -> io::Result<()> {
            crate::path_safety::atomic_write_file(&self.path.join(leaf), bytes)
        }
    }
}
