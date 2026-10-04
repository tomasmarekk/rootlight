//! Bounded Windows names and full object identities from retained handles.
//!
//! The VFS uses these observations only to attest a capability-relative reopen;
//! handle names are never ambient filesystem authority.

#![allow(
    unsafe_code,
    reason = "Windows handle name and full-width identity APIs have no safe standard-library wrapper"
)]

use std::{
    ffi::OsString,
    io,
    os::windows::{ffi::OsStringExt as _, io::AsRawHandle},
    path::PathBuf,
};

use windows::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{
        FILE_ID_INFO, FILE_NAME_NORMALIZED, FileIdInfo, GETFINALPATHNAMEBYHANDLE_FLAGS,
        GetFileInformationByHandleEx, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    },
};

use crate::{MAX_PATH_BYTES, VfsError, controlled};

// An absolute handle name combines the root and relative-path envelopes, one
// separator, and at most the extended UNC prefix. Relative admission is still
// checked separately by RelativePath; this is only a metadata-buffer bound.
const MAX_HANDLE_NAME_BYTES: usize = MAX_PATH_BYTES * 2 + 2 + 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HandleIdentity {
    volume: u64,
    file: [u8; 16],
}

pub(super) fn same_object_and_name(
    original_identity: HandleIdentity,
    original_name: &std::path::Path,
    reopened_identity: HandleIdentity,
    reopened_name: &std::path::Path,
) -> bool {
    original_identity == reopened_identity && original_name == reopened_name
}

pub(super) fn identity(
    handle: &impl AsRawHandle,
    check: &mut impl FnMut() -> Result<(), VfsError>,
) -> Result<HandleIdentity, VfsError> {
    let mut info = FILE_ID_INFO::default();
    let bytes =
        u32::try_from(size_of::<FILE_ID_INFO>()).map_err(|_| VfsError::MemoryUnavailable)?;
    controlled(check, || {
        // SAFETY: The borrowed owner keeps the handle valid. `info` has the
        // FILE_ID_INFO C layout and exactly the writable byte size passed here.
        unsafe {
            GetFileInformationByHandleEx(
                HANDLE(handle.as_raw_handle()),
                FileIdInfo,
                std::ptr::from_mut(&mut info).cast(),
                bytes,
            )
        }
        .map_err(|source| VfsError::ReadFile {
            source: io::Error::other(source),
        })
    })?;
    Ok(HandleIdentity {
        volume: info.VolumeSerialNumber,
        file: info.FileId.Identifier,
    })
}

pub(super) fn normalized_name(
    handle: &impl AsRawHandle,
    check: &mut impl FnMut() -> Result<(), VfsError>,
) -> Result<PathBuf, VfsError> {
    const MAX_NAME_UNITS: usize = MAX_HANDLE_NAME_BYTES / size_of::<u16>();
    let mut short = [0u16; 512];
    let length = query_name(handle, &mut short, check)?;
    if length < short.len() {
        return bounded_path(&short, length, check);
    }
    if length > MAX_NAME_UNITS + 1 {
        return Err(VfsError::PathTooLong {
            maximum: MAX_HANDLE_NAME_BYTES,
        });
    }
    let mut name = Vec::new();
    controlled(check, || {
        name.try_reserve_exact(length)
            .map_err(|_| VfsError::MemoryUnavailable)
    })?;
    name.resize(length, 0);
    let length = query_name(handle, &mut name, check)?;
    if length >= name.len() {
        return Err(VfsError::UnstableFile);
    }
    bounded_path(&name, length, check)
}

fn query_name(
    handle: &impl AsRawHandle,
    buffer: &mut [u16],
    check: &mut impl FnMut() -> Result<(), VfsError>,
) -> Result<usize, VfsError> {
    controlled(check, || {
        // SAFETY: The borrowed owner keeps the handle valid. The writable slice
        // is initialized, bounded below u32::MAX, and exclusively borrowed for
        // the native call; Windows writes at most the supplied slice length.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                HANDLE(handle.as_raw_handle()),
                buffer,
                GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0),
            )
        };
        if length == 0 {
            return Err(VfsError::ReadFile {
                source: io::Error::last_os_error(),
            });
        }
        usize::try_from(length).map_err(|_| VfsError::PathTooLong {
            maximum: MAX_HANDLE_NAME_BYTES,
        })
    })
}

fn bounded_path(
    buffer: &[u16],
    length: usize,
    check: &mut impl FnMut() -> Result<(), VfsError>,
) -> Result<PathBuf, VfsError> {
    let name = buffer.get(..length).ok_or(VfsError::UnstableFile)?;
    if name.contains(&0) || length > MAX_HANDLE_NAME_BYTES / size_of::<u16>() {
        return Err(VfsError::InvalidRelativePath);
    }
    controlled(check, || Ok(PathBuf::from(OsString::from_wide(name))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn reopen_attestation_requires_full_identity_volume_and_exact_name() {
        let original = HandleIdentity {
            volume: 1,
            file: [0; 16],
        };
        let name = Path::new(r"\\?\C:\root\Unit.fs");
        assert!(same_object_and_name(original, name, original, name));
        let mut high_bits = original;
        high_bits.file[15] = 1;
        assert!(!same_object_and_name(original, name, high_bits, name));
        assert!(!same_object_and_name(
            original,
            name,
            HandleIdentity {
                volume: 2,
                ..original
            },
            name,
        ));
        for other in [r"\\?\C:\root\unit.fs", r"\\?\C:\root\UntrackedLink.fs"] {
            assert!(!same_object_and_name(
                original,
                name,
                original,
                Path::new(other)
            ));
        }
    }

    #[test]
    fn handle_name_conversion_rejects_overflow_and_embedded_nul() {
        let mut check = || Ok(());
        assert!(matches!(
            bounded_path(&[65], 2, &mut check),
            Err(VfsError::UnstableFile)
        ));
        assert!(matches!(
            bounded_path(&[65, 0, 66], 3, &mut check),
            Err(VfsError::InvalidRelativePath)
        ));
        assert!(matches!(
            bounded_path(
                &[65; MAX_HANDLE_NAME_BYTES / 2 + 1],
                MAX_HANDLE_NAME_BYTES / 2 + 1,
                &mut check
            ),
            Err(VfsError::InvalidRelativePath),
        ));
    }

    #[test]
    fn absolute_handle_names_can_combine_individually_bounded_paths() {
        let mut check = || Ok(());
        let name = [65; MAX_PATH_BYTES / 2 + 1];
        assert!(bounded_path(&name, name.len(), &mut check).is_ok());
    }
}
