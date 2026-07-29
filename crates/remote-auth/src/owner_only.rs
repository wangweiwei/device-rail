#[cfg(target_os = "macos")]
use std::{
    ffi::{c_int, c_void},
    fs::File,
    os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
    path::Path,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExtendedAclError {
    #[cfg(target_os = "macos")]
    Present,
    #[cfg(target_os = "macos")]
    Unavailable,
}

#[cfg(target_os = "macos")]
const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
#[cfg(target_os = "macos")]
const ACL_FIRST_ENTRY: c_int = 0;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> *mut c_void;
    fn acl_get_file(path: *const i8, acl_type: c_int) -> *mut c_void;
    fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
    fn acl_free(object: *mut c_void) -> c_int;
}

#[cfg(target_os = "macos")]
struct Acl(*mut c_void);

#[cfg(target_os = "macos")]
impl Drop for Acl {
    fn drop(&mut self) {
        // SAFETY: this is the unique owner of a non-null acl_get_* result.
        let _ = unsafe { acl_free(self.0) };
    }
}

#[cfg(target_os = "macos")]
fn require_empty(acl: *mut c_void) -> Result<(), ExtendedAclError> {
    if acl.is_null() {
        return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            // Darwin reports ENOENT when the object has no extended ACL.
            Ok(())
        } else {
            Err(ExtendedAclError::Unavailable)
        };
    }
    let acl = Acl(acl);
    let mut entry = std::ptr::null_mut();
    // SAFETY: the ACL and output pointer are valid for this call.
    match unsafe { acl_get_entry(acl.0, ACL_FIRST_ENTRY, &mut entry) } {
        0 => Err(ExtendedAclError::Present),
        _ => Err(ExtendedAclError::Unavailable),
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn require_no_extended_acl_path(path: &Path) -> Result<(), ExtendedAclError> {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| ExtendedAclError::Unavailable)?;
    // SAFETY: `path` is NUL-terminated and live for the call.
    require_empty(unsafe { acl_get_file(path.as_ptr(), ACL_TYPE_EXTENDED) })
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn require_no_extended_acl_path(
    _path: &std::path::Path,
) -> Result<(), ExtendedAclError> {
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn require_no_extended_acl_file(file: &File) -> Result<(), ExtendedAclError> {
    // SAFETY: the descriptor remains borrowed and valid for the call.
    require_empty(unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) })
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn require_no_extended_acl_file(_file: &std::fs::File) -> Result<(), ExtendedAclError> {
    Ok(())
}
