//! Descriptor-bound extended ACL enforcement for private SQLite files.

#![allow(
    unsafe_code,
    reason = "macOS descriptor ACL APIs have no safe standard-library or rustix wrapper"
)]

#[cfg(target_os = "macos")]
use std::{ffi::c_void, io};

#[cfg(target_os = "macos")]
use crate::{CatalogError, CatalogErrorKind};

#[cfg(target_os = "macos")]
const MACOS_ACL_TYPE_EXTENDED: u32 = 256;
#[cfg(target_os = "macos")]
const MACOS_FILESEC_ACL: std::ffi::c_int = 5;
#[cfg(target_os = "macos")]
const MACOS_FILESEC_REMOVE_ACL_ADDRESS: usize = 1;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn acl_free(object: *mut c_void) -> std::ffi::c_int;
    fn acl_get_fd_np(fd: std::ffi::c_int, acl_type: u32) -> *mut c_void;
    fn fchmodx_np(fd: std::ffi::c_int, file_security: *mut c_void) -> std::ffi::c_int;
    fn filesec_free(file_security: *mut c_void);
    fn filesec_init() -> *mut c_void;
    fn filesec_set_property(
        file_security: *mut c_void,
        property: std::ffi::c_int,
        value: *const c_void,
    ) -> std::ffi::c_int;
}

#[cfg(target_os = "macos")]
pub(super) fn clear_extended_acl<H: std::os::fd::AsRawFd>(handle: &H) -> Result<(), CatalogError> {
    // SAFETY: `filesec_init` creates a process-owned opaque allocation. A
    // non-null result is released exactly once below with `filesec_free`.
    let file_security = unsafe { filesec_init() };
    if file_security.is_null() {
        return Err(storage_error());
    }

    let remove_acl = std::ptr::without_provenance::<c_void>(MACOS_FILESEC_REMOVE_ACL_ADDRESS);
    // SAFETY: `file_security` is the live allocation returned above. Darwin
    // defines pointer value 1 as the non-dereferenced ACL-removal sentinel for
    // `FILESEC_ACL`; the descriptor is retained by `handle` for `fchmodx_np`.
    let removal_error = unsafe {
        let set_result = filesec_set_property(file_security, MACOS_FILESEC_ACL, remove_acl);
        let error = if set_result != 0 {
            Some(io::Error::last_os_error())
        } else if fchmodx_np(handle.as_raw_fd(), file_security) != 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        filesec_free(file_security);
        error
    };
    if let Some(error) = removal_error {
        return Err(CatalogError::io(CatalogErrorKind::Storage, error));
    }
    verify_no_extended_acl(handle)
}

#[cfg(target_os = "macos")]
pub(super) fn verify_no_extended_acl<H: std::os::fd::AsRawFd>(
    handle: &H,
) -> Result<(), CatalogError> {
    // SAFETY: the descriptor remains valid for the call and Darwin returns
    // either a process-owned ACL allocation or null with errno.
    let acl = unsafe { acl_get_fd_np(handle.as_raw_fd(), MACOS_ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(CatalogError::io(CatalogErrorKind::Storage, error))
        };
    }

    // SAFETY: `acl` is the non-null allocation returned immediately above and
    // this is its single release.
    let free_result = unsafe { acl_free(acl) };
    if free_result != 0 {
        return Err(storage_error());
    }
    Err(CatalogError::new(CatalogErrorKind::InsecureFile))
}

#[cfg(target_os = "macos")]
fn storage_error() -> CatalogError {
    CatalogError::io(CatalogErrorKind::Storage, io::Error::last_os_error())
}
