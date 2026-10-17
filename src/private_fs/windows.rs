use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_NO_TOKEN, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::*;

const ACCESS_ALLOWED_ACE_TYPE_BYTE: u8 = 0;

fn checked(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) struct PrivateSecurity {
    descriptor: SECURITY_DESCRIPTOR,
    _acl: Vec<usize>,
    user: Vec<usize>,
    ace_flags: u8,
}

impl PrivateSecurity {
    pub(crate) fn new(inherit_children: bool) -> io::Result<Self> {
        let mut token = std::ptr::null_mut();
        unsafe {
            if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
                    return Err(error);
                }
                checked(OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_QUERY,
                    &mut token,
                ))?;
            }
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut length = 0;
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &mut length,
            );
        }
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut user = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
        unsafe {
            checked(GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                user.as_mut_ptr().cast(),
                length,
                &mut length,
            ))?;
            let sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            if IsValidSid(sid) == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "effective token contains an invalid user SID",
                ));
            }
            let size = std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>()
                - std::mem::size_of::<u32>()
                + GetLengthSid(sid) as usize;
            let mut acl = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
            let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
            checked(InitializeAcl(acl_ptr, size as u32, ACL_REVISION))?;
            let flags = if inherit_children {
                CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE
            } else {
                0
            };
            checked(AddAccessAllowedAceEx(
                acl_ptr,
                ACL_REVISION,
                flags,
                FILE_ALL_ACCESS,
                sid,
            ))?;
            let mut descriptor = SECURITY_DESCRIPTOR::default();
            let descriptor_ptr = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
            checked(InitializeSecurityDescriptor(descriptor_ptr, 1))?;
            checked(SetSecurityDescriptorOwner(descriptor_ptr, sid, 0))?;
            checked(SetSecurityDescriptorDacl(descriptor_ptr, 1, acl_ptr, 0))?;
            checked(SetSecurityDescriptorControl(
                descriptor_ptr,
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            ))?;
            Ok(Self {
                descriptor,
                _acl: acl,
                user,
                ace_flags: flags as u8,
            })
        }
    }

    fn sid(&self) -> PSID {
        unsafe { (*self.user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    pub(crate) fn descriptor_ptr(&mut self) -> PSECURITY_DESCRIPTOR {
        (&mut self.descriptor as *mut SECURITY_DESCRIPTOR).cast()
    }

    fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor_ptr(),
            bInheritHandle: 0,
        }
    }
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let absolute = std::path::absolute(path)?;
    let mut raw = absolute.as_os_str().encode_wide().collect::<Vec<_>>();
    if raw.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private path contains NUL",
        ));
    }
    const VERBATIM: [u16; 4] = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const DEVICE: [u16; 4] = [b'\\' as u16, b'\\' as u16, b'.' as u16, b'\\' as u16];
    if raw.starts_with(&VERBATIM) || raw.starts_with(&DEVICE) {
        raw.push(0);
        return Ok(raw);
    }
    let mut wide = VERBATIM.to_vec();
    if raw.starts_with(&[b'\\' as u16, b'\\' as u16]) {
        wide.extend("UNC\\".encode_utf16());
        wide.extend_from_slice(&raw[2..]);
    } else {
        wide.extend(raw);
    }
    wide.push(0);
    Ok(wide)
}

fn security_descriptor(handle: HANDLE) -> io::Result<Vec<usize>> {
    let mut needed = 0;
    unsafe {
        GetKernelObjectSecurity(
            handle,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut descriptor = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    unsafe {
        checked(GetKernelObjectSecurity(
            handle,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            descriptor.as_mut_ptr().cast(),
            needed,
            &mut needed,
        ))?;
    }
    Ok(descriptor)
}

fn verify_handle(handle: HANDLE, expected: &PrivateSecurity) -> io::Result<()> {
    let descriptor = security_descriptor(handle)?;
    let descriptor_ptr = descriptor.as_ptr().cast_mut().cast();
    unsafe {
        let mut owner = std::ptr::null_mut();
        let mut owner_defaulted = 0;
        checked(GetSecurityDescriptorOwner(
            descriptor_ptr,
            &mut owner,
            &mut owner_defaulted,
        ))?;
        if owner.is_null() || EqualSid(owner, expected.sid()) == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private Windows object has the wrong owner SID",
            ));
        }

        let mut control = 0;
        let mut revision = 0;
        checked(GetSecurityDescriptorControl(
            descriptor_ptr,
            &mut control,
            &mut revision,
        ))?;
        if control & SE_DACL_PROTECTED == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private Windows object has an inheritable DACL",
            ));
        }

        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = std::ptr::null_mut();
        checked(GetSecurityDescriptorDacl(
            descriptor_ptr,
            &mut present,
            &mut acl,
            &mut defaulted,
        ))?;
        if present == 0 || acl.is_null() || (*acl).AceCount != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private Windows object does not have exactly one DACL ACE",
            ));
        }
        let mut raw_ace = std::ptr::null_mut();
        checked(GetAce(acl, 0, &mut raw_ace))?;
        let ace = raw_ace.cast::<ACCESS_ALLOWED_ACE>();
        if (*ace).Header.AceType != ACCESS_ALLOWED_ACE_TYPE_BYTE
            || (*ace).Header.AceFlags != expected.ace_flags
            || (*ace).Mask != FILE_ALL_ACCESS
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private Windows object has an unexpected DACL ACE",
            ));
        }
        let sid = std::ptr::addr_of_mut!((*ace).SidStart).cast();
        if EqualSid(sid, expected.sid()) == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private Windows object grants a different SID",
            ));
        }
    }
    Ok(())
}

pub(crate) fn verify_private_handle(handle: HANDLE, directory: bool) -> io::Result<()> {
    let expected = PrivateSecurity::new(directory)?;
    verify_handle(handle, &expected)
}

fn open_for_verification(path: &Path, directory: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    if directory {
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    }
    options.open(path)
}

fn verify_path(path: &Path, directory: bool) -> io::Result<()> {
    let file = open_for_verification(path, directory)?;
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || metadata.is_dir() != directory
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "private Windows path has the wrong object type: {}",
                path.display()
            ),
        ));
    }
    let expected = PrivateSecurity::new(directory)?;
    verify_handle(file.as_raw_handle(), &expected)
}

fn privatize_path(path: &Path, directory: bool) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options
        .access_mode(READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    if directory {
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || metadata.is_dir() != directory
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private Windows object has the wrong type",
        ));
    }
    let mut security = PrivateSecurity::new(directory)?;
    let current = security_descriptor(file.as_raw_handle())?;
    let descriptor_ptr = current.as_ptr().cast_mut().cast();
    unsafe {
        let mut owner = std::ptr::null_mut();
        let mut defaulted = 0;
        checked(GetSecurityDescriptorOwner(
            descriptor_ptr,
            &mut owner,
            &mut defaulted,
        ))?;
        if owner.is_null() || EqualSid(owner, security.sid()) == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to privatize Windows object owned by another SID",
            ));
        }
        checked(SetKernelObjectSecurity(
            file.as_raw_handle(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            security.descriptor_ptr(),
        ))?;
    }
    verify_handle(file.as_raw_handle(), &security)
}

pub(crate) fn verify_private_dir(path: &Path) -> io::Result<()> {
    verify_path(path, true)
}

pub(crate) fn verify_private_file(path: &Path) -> io::Result<()> {
    verify_path(path, false)
}

pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    let path_wide = wide_path(path)?;
    let mut security = PrivateSecurity::new(true)?;
    let attributes = security.attributes();
    let ok = unsafe { CreateDirectoryW(path_wide.as_ptr(), &attributes) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if let Err(error) = verify_private_dir(path) {
        let _ = std::fs::remove_dir(path);
        return Err(error);
    }
    Ok(())
}

fn create_or_open_file(path: &Path, disposition: u32, append: bool) -> io::Result<File> {
    let path_wide = wide_path(path)?;
    let mut security = PrivateSecurity::new(false)?;
    let attributes = security.attributes();
    let access = if append {
        FILE_APPEND_DATA | FILE_GENERIC_READ | READ_CONTROL
    } else {
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | READ_CONTROL
    };
    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            access,
            if append {
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
            } else {
                FILE_SHARE_READ
            },
            &attributes,
            disposition,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    if let Err(error) = verify_handle(file.as_raw_handle(), &security) {
        drop(file);
        if disposition == CREATE_NEW {
            let _ = std::fs::remove_file(path);
        }
        return Err(error);
    }
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        drop(file);
        if disposition == CREATE_NEW {
            let _ = std::fs::remove_file(path);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private Windows file is a reparse point",
        ));
    }
    Ok(file)
}

pub(crate) fn create_private_file(path: &Path) -> io::Result<File> {
    create_or_open_file(path, CREATE_NEW, false)
}

pub(crate) fn open_private_append(path: &Path) -> io::Result<File> {
    create_or_open_file(path, OPEN_ALWAYS, true)
}

pub(crate) fn privatize_file(path: &Path) -> io::Result<()> {
    privatize_path(path, false)
}

pub(crate) fn privatize_dir(path: &Path) -> io::Result<()> {
    privatize_path(path, true)
}
