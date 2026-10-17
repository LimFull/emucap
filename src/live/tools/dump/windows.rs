use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FileRenameInformation, NtCreateFile, NtSetInformationFile, FILE_CREATE, FILE_DIRECTORY_FILE,
    FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION,
    FILE_SYNCHRONOUS_IO_NONALERT,
};
use windows_sys::Win32::Foundation::{RtlNtStatusToDosError, OBJ_CASE_INSENSITIVE, UNICODE_STRING};
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use crate::private_fs::windows::PrivateSecurity;

pub(super) struct Directory {
    file: File,
    pub path: PathBuf,
    // Keep every canonical ancestor from being renamed while the adapter uses its pathname.
    // Sharing modes do not deny attribute writes; owned staging uses a private creation DACL.
    _ancestors: Vec<File>,
}

fn open_path(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY)
        // Retained directory handles must allow ordinary child creation and writes. Omitting
        // FILE_SHARE_DELETE still pins each directory's namespace binding against replacement.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

fn real_directory(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "dump parent must resolve to a real directory",
        ))
    }
}

fn relative_file(
    parent: &File,
    name: &OsStr,
    create: bool,
    options: u32,
    access: u32,
) -> io::Result<File> {
    let mut encoded: Vec<u16> = name.encode_wide().collect();
    let length = u16::try_from(encoded.len().saturating_mul(2))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "dump member name is too long"))?;
    if encoded.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in dump member name",
        ));
    }
    let name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: encoded.as_mut_ptr(),
    };
    let directory = options & FILE_DIRECTORY_FILE != 0;
    let mut security = if create {
        Some(PrivateSecurity::new(directory)?)
    } else {
        None
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: security.as_mut().map_or(std::ptr::null(), |value| {
            value
                .descriptor_ptr()
                .cast::<windows_sys::Win32::Security::SECURITY_DESCRIPTOR>()
                .cast_const()
        }),
        ..Default::default()
    };
    let mut handle = std::ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // FILE_CREATE atomically creates AND acquires the object. No create-path/open-path gap can
    // give cleanup ownership of an unrelated directory. Reject reparse traversal at the leaf.
    let share_mode = if directory {
        FILE_SHARE_READ | FILE_SHARE_WRITE
    } else {
        FILE_SHARE_READ
    };
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access | READ_CONTROL | SYNCHRONIZE,
            &attributes,
            &mut status_block,
            std::ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            share_mode,
            if create { FILE_CREATE } else { FILE_OPEN },
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    // SAFETY: NtCreateFile returned a new owned kernel handle.
    let file = unsafe { File::from_raw_handle(handle) };
    if create {
        crate::private_fs::windows::verify_private_handle(file.as_raw_handle(), directory)?;
    }
    Ok(file)
}

fn delete_handle(file: &File) -> io::Result<()> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            &info as *const _ as *const _,
            std::mem::size_of_val(&info) as u32,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl Directory {
    pub fn open_parent(path: &Path) -> io::Result<Self> {
        let mut ancestors = Vec::new();
        for path in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
            let file = open_path(path)?;
            real_directory(&file)?;
            ancestors.push(file);
        }
        let file = ancestors
            .pop()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing dump parent"))?;
        Ok(Self {
            file,
            path: path.into(),
            _ancestors: ancestors,
        })
    }

    pub fn create_dir(&self, name: &OsStr) -> io::Result<Self> {
        let file = relative_file(
            &self.file,
            name,
            true,
            FILE_DIRECTORY_FILE,
            DELETE | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
        )?;
        Ok(Self {
            file,
            path: self.path.join(name),
            _ancestors: Vec::new(),
        })
    }

    pub fn check_binding(&self) -> io::Result<()> {
        for ancestor in &self._ancestors {
            real_directory(ancestor)?;
        }
        real_directory(&self.file)
    }

    pub fn write_new_file(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let mut file = relative_file(
            &self.file,
            name.as_ref(),
            true,
            FILE_NON_DIRECTORY_FILE,
            FILE_GENERIC_WRITE,
        )?;
        file.write_all(bytes)?;
        file.sync_all()
    }

    pub fn publish_child(
        &self,
        child: &Self,
        _name: &OsStr,
        parent: &Self,
        destination: &OsStr,
    ) -> io::Result<()> {
        let name: Vec<u16> = destination.encode_wide().collect();
        let bytes = std::mem::size_of::<FILE_RENAME_INFORMATION>() + name.len() * 2;
        // A word buffer provides native rename-info alignment, including its variable name tail.
        let mut buffer = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        // SAFETY: buffer is aligned and large enough for the header and complete wide name.
        unsafe {
            (*info).Anonymous.ReplaceIfExists = false;
            (*info).RootDirectory = parent.file.as_raw_handle();
            (*info).FileNameLength = (name.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
        }
        let mut status_block = IO_STATUS_BLOCK::default();
        let status = unsafe {
            NtSetInformationFile(
                child.file.as_raw_handle(),
                &mut status_block,
                info.cast(),
                bytes as u32,
                FileRenameInformation,
            )
        };
        if status < 0 {
            Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(status) } as i32,
            ))
        } else {
            Ok(())
        }
    }

    pub fn remove_empty_child(&self, _name: &OsStr, child: Self) -> io::Result<()> {
        // Deletion is bound to the acquired handle, never to a reopened caller pathname.
        delete_handle(&child.file)
    }

    pub fn remove_contents(&self, remaining: &mut usize, depth: usize) -> io::Result<()> {
        if depth > 64 {
            return Err(io::Error::other("dump cleanup directory depth exceeded"));
        }
        for entry in std::fs::read_dir(&self.path)? {
            let entry = entry?;
            *remaining = remaining
                .checked_sub(1)
                .ok_or_else(|| io::Error::other("dump cleanup entry limit exceeded"))?;
            let file = relative_file(
                &self.file,
                &entry.file_name(),
                false,
                0,
                DELETE | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
            )?;
            let metadata = file.metadata()?;
            if metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                let child = Self {
                    file,
                    path: entry.path(),
                    _ancestors: Vec::new(),
                };
                child.remove_contents(remaining, depth + 1)?;
                delete_handle(&child.file)?;
            } else {
                delete_handle(&file)?;
            }
        }
        Ok(())
    }
}
