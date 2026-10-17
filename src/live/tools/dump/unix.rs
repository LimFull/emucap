use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(super) struct Directory {
    file: File,
    pub path: PathBuf,
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in dump path"))
}

fn result(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn from_fd(fd: libc::c_int) -> io::Result<File> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: the successful open returned a new, exclusively owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

impl Directory {
    pub fn open_parent(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        // A protected parent is required while mkdirat's new entry is acquired. A sticky /tmp
        // owned by root is valid; an arbitrary other-user/writable parent cannot confer ownership.
        let uid = unsafe { libc::geteuid() };
        if metadata.uid() != 0 && metadata.uid() != uid
            || metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0
        {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied,
                "dump parent must be user-owned or root-owned and protected from other-user entry replacement"));
        }
        Ok(Self {
            file,
            path: path.into(),
        })
    }

    fn open_child(&self, name: &OsStr) -> io::Result<Self> {
        let name_c = c_name(name)?;
        // SAFETY: parent descriptor and NUL-terminated name remain live; no symlink traversal.
        let file = from_fd(unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        Ok(Self {
            file,
            path: self.path.join(name),
        })
    }

    pub fn create_dir(&self, name: &OsStr) -> io::Result<Self> {
        let name_c = c_name(name)?;
        // SAFETY: live parent descriptor and valid NUL-terminated leaf name; 0700 at creation.
        result(unsafe { libc::mkdirat(self.file.as_raw_fd(), name_c.as_ptr(), 0o700) })?;
        // Do not clean a pathname if acquisition fails. There is no owned handle to authorize it.
        let acquisition_error = |e: io::Error| {
            io::Error::new(
                e.kind(),
                format!(
                    "created staging entry could not be acquired: {e}; inspect {}",
                    self.path.join(name).display()
                ),
            )
        };
        let child = self.open_child(name).map_err(acquisition_error)?;
        let metadata = child.file.metadata().map_err(acquisition_error)?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "dump staging ownership changed; inspect {}",
                    child.path.display()
                ),
            ));
        }
        Ok(child)
    }

    pub fn check_binding(&self) -> io::Result<()> {
        let current = std::fs::symlink_metadata(&self.path)?;
        let owned = self.file.metadata()?;
        if current.is_dir() && current.dev() == owned.dev() && current.ino() == owned.ino() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "dump directory path changed identity",
            ))
        }
    }

    pub fn write_new_file(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let name = c_name(name.as_ref())?;
        // SAFETY: owned directory anchors the write; create-new never follows/replaces a member.
        let mut file = from_fd(unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        file.write_all(bytes)?;
        file.sync_all()
    }

    pub fn publish_child(
        &self,
        child: &Self,
        name: &OsStr,
        parent: &Self,
        destination: &OsStr,
    ) -> io::Result<()> {
        self.check_child(name, child)?;
        let source = c_name(name)?;
        let destination = c_name(destination)?;
        // Both namespaces are anchored to retained directory descriptors. The source namespace is
        // inside our private container; the destination cannot be replaced, even if it appears now.
        #[cfg(target_os = "macos")]
        let rc = unsafe {
            libc::renameatx_np(
                self.file.as_raw_fd(),
                source.as_ptr(),
                parent.file.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        let rc = unsafe {
            libc::renameat2(
                self.file.as_raw_fd(),
                source.as_ptr(),
                parent.file.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exclusive dump directory publication is unavailable",
        ));
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        result(rc)
    }

    fn check_child(&self, name: &OsStr, child: &Self) -> io::Result<()> {
        let current = self.open_child(name)?.file.metadata()?;
        let owned = child.file.metadata()?;
        if current.dev() == owned.dev() && current.ino() == owned.ino() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "owned dump directory entry was replaced",
            ))
        }
    }

    pub fn remove_empty_child(&self, name: &OsStr, child: Self) -> io::Result<()> {
        self.check_child(name, &child)?;
        let name = c_name(name)?;
        // Only unlink an empty directory entry. Recursive cleanup has already used child's handle.
        result(unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) })
    }

    pub fn remove_contents(&self, remaining: &mut usize, depth: usize) -> io::Result<()> {
        if depth > 64 {
            return Err(io::Error::other("dump cleanup directory depth exceeded"));
        }
        for name in self.names(remaining)? {
            match self.open_child(&name) {
                Ok(child) => {
                    child.remove_contents(remaining, depth + 1)?;
                    self.remove_empty_child(&name, child)?;
                }
                Err(e) if matches!(e.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) => {
                    let name = c_name(&name)?;
                    // unlinkat does not follow symlinks; an injected directory causes failure.
                    result(unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) })?;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn names(&self, remaining: &mut usize) -> io::Result<Vec<OsString>> {
        // openat(".") gives readdir its own offset and descriptor, retaining the cleanup root.
        let duplicate = self.open_child(".".as_ref())?;
        let stream = unsafe { libc::fdopendir(duplicate.file.as_raw_fd()) };
        if stream.is_null() {
            return Err(io::Error::last_os_error());
        }
        use std::os::fd::IntoRawFd;
        let _ = duplicate.file.into_raw_fd(); // fdopendir now owns it.
        struct Stream(*mut libc::DIR);
        impl Drop for Stream {
            fn drop(&mut self) {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let stream = Stream(stream);
        let mut names = Vec::new();
        loop {
            #[cfg(target_os = "macos")]
            unsafe {
                *libc::__error() = 0;
            }
            #[cfg(target_os = "linux")]
            unsafe {
                *libc::__errno_location() = 0;
            }
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(0) {
                    Ok(names)
                } else {
                    Err(error)
                };
            }
            // SAFETY: readdir's entry is live until the next call; copy the NUL-terminated name.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            *remaining = remaining
                .checked_sub(1)
                .ok_or_else(|| io::Error::other("dump cleanup entry limit exceeded"))?;
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
}
