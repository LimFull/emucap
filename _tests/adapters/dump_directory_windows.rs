//! Standalone Windows runtime checks of the production directory-handle implementation.
//! Build with windows-sys features matching Cargo.toml; run on a local Windows filesystem.
#[path = "../../src/live/tools/dump/windows.rs"]
mod platform;

use platform::Directory;
use std::io;

fn main() -> io::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "emucap-dump-handles-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root)?;
    let result = check(&root);
    // Only the fixture root created exclusively above belongs to this standalone test.
    let cleanup = std::fs::remove_dir_all(&root);
    result?;
    cleanup?;
    println!("Windows dump handles: create-new, exclusive publish, namespace locks, cleanup and sentinel preservation passed");
    Ok(())
}

fn check(root: &std::path::Path) -> io::Result<()> {
    let restricted = shared_parent_and_restricted_writer(root)?;
    println!("open parent");
    let parent = Directory::open_parent(root)?;
    println!("create container");
    let container = parent.create_dir("container".as_ref())?;
    assert!(parent.create_dir("container".as_ref()).is_err());
    let payload = container.create_dir("payload".as_ref())?;
    private_staging_excludes_restricted_writer(root, &payload, &restricted)?;
    println!("write and namespace exclusion");
    payload.check_binding()?;
    payload.write_new_file("state.json", b"{}")?;
    assert!(payload
        .write_new_file("state.json", b"replacement")
        .is_err());
    assert!(std::fs::rename(root, root.with_extension("moved")).is_err());
    assert!(std::fs::rename(&payload.path, payload.path.with_extension("moved")).is_err());
    std::fs::create_dir(root.join("occupied"))?;
    std::fs::write(root.join("occupied/keep"), b"KEEP")?;
    assert!(container
        .publish_child(&payload, "payload".as_ref(), &parent, "occupied".as_ref())
        .is_err());
    assert_eq!(std::fs::read(root.join("occupied/keep"))?, b"KEEP");
    std::fs::create_dir(root.join("empty"))?;
    assert!(container
        .publish_child(&payload, "payload".as_ref(), &parent, "empty".as_ref())
        .is_err());
    println!("publish to absent destination");
    container.publish_child(&payload, "payload".as_ref(), &parent, "published".as_ref())?;
    println!("cleanup empty container");
    drop(payload);
    parent.remove_empty_child("container".as_ref(), container)?;
    assert_eq!(std::fs::read(root.join("published/state.json"))?, b"{}");
    let failed = parent.create_dir("failed".as_ref())?;
    println!("recursive cleanup");
    failed.write_new_file("one.bin", b"one")?;
    let nested = failed.create_dir("nested".as_ref())?;
    nested.write_new_file("two.bin", b"two")?;
    drop(nested);
    failed.remove_contents(&mut 100, 0)?;
    parent.remove_empty_child("failed".as_ref(), failed)?;
    assert!(!root.join("failed").exists());
    Ok(())
}

fn checked(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// The second access check on this restricted token requires an Everyone ACE. Thus it can write
// the deliberately shared fixture parent, but cannot use the service account's staging ACE.
fn shared_parent_and_restricted_writer(
    root: &std::path::Path,
) -> io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::Threading::*;
    unsafe {
        let mut world = [0u64; 9];
        let sid = world.as_mut_ptr().cast();
        let mut size = std::mem::size_of_val(&world) as u32;
        checked(CreateWellKnownSid(
            WinWorldSid,
            std::ptr::null_mut(),
            sid,
            &mut size,
        ))?;
        let mut acl = [0usize; 32];
        let ptr = acl.as_mut_ptr().cast::<ACL>();
        checked(InitializeAcl(
            ptr,
            std::mem::size_of_val(&acl) as u32,
            ACL_REVISION,
        ))?;
        checked(AddAccessAllowedAceEx(
            ptr,
            ACL_REVISION,
            CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
            FILE_ALL_ACCESS,
            sid,
        ))?;
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let sd = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
        checked(InitializeSecurityDescriptor(sd, 1))?;
        checked(SetSecurityDescriptorDacl(sd, 1, ptr, 0))?;
        let path: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
        checked(SetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            sd,
        ))?;
        let mut token = std::ptr::null_mut();
        checked(OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE,
            &mut token,
        ))?;
        let token = OwnedHandle::from_raw_handle(token);
        let restriction = SID_AND_ATTRIBUTES {
            Sid: sid,
            Attributes: 0,
        };
        let mut restricted = std::ptr::null_mut();
        checked(CreateRestrictedToken(
            token.as_raw_handle(),
            DISABLE_MAX_PRIVILEGE,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            1,
            &restriction,
            &mut restricted,
        ))?;
        Ok(OwnedHandle::from_raw_handle(restricted))
    }
}

fn private_staging_excludes_restricted_writer(
    root: &std::path::Path,
    payload: &Directory,
    token: &std::os::windows::io::OwnedHandle,
) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Security::{ImpersonateLoggedOnUser, RevertToSelf};
    use windows_sys::Win32::Storage::FileSystem::*;
    struct Revert;
    impl Drop for Revert {
        fn drop(&mut self) {
            assert_ne!(unsafe { RevertToSelf() }, 0);
        }
    }
    checked(unsafe { ImpersonateLoggedOnUser(token.as_raw_handle()) })?;
    let _revert = Revert;
    // Positive control: this principal can create and populate an unrelated directory.
    let foreign = root.join("foreign");
    std::fs::create_dir(&foreign)?;
    std::fs::write(foreign.join("keep"), b"KEEP")?;
    assert_eq!(
        std::fs::rename(&foreign, payload.path.join("injected"))
            .unwrap_err()
            .raw_os_error(),
        Some(5)
    );
    // Attribute access bypasses sharing exclusions, so this must be denied by the creation DACL.
    let attributes = std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&payload.path);
    assert_eq!(attributes.unwrap_err().raw_os_error(), Some(5));
    assert_eq!(std::fs::read(foreign.join("keep"))?, b"KEEP");
    Ok(())
}
