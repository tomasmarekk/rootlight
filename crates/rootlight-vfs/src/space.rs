use std::{io, path::Path};

/// Returns the bytes available to the current process on the path's volume.
///
/// # Errors
///
/// Returns an I/O error when the operating system cannot query the volume or
/// its reported byte count cannot be represented.
#[cfg(unix)]
pub fn available_space(path: &Path) -> io::Result<u64> {
    let statistics = rustix::fs::statvfs(path).map_err(io::Error::from)?;
    statistics
        .f_frsize
        .checked_mul(statistics.f_bavail)
        .ok_or_else(|| io::Error::other("available filesystem space overflowed u64"))
}

/// Returns the bytes available to the current process on the path's volume.
///
/// # Errors
///
/// Returns an I/O error when the operating system cannot query the volume.
#[cfg(windows)]
pub fn available_space(path: &Path) -> io::Result<u64> {
    crate::platform::available_volume_space(path)
}

#[cfg(test)]
mod tests {
    use super::available_space;

    #[test]
    fn queries_existing_directory() {
        let directory = tempfile::tempdir().expect("temporary directory exists");
        assert!(available_space(directory.path()).expect("volume can be queried") > 0);
        assert!(available_space(&directory.path().join("missing")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn rejects_embedded_nul_before_querying_the_volume() {
        let error = available_space(std::path::Path::new("other\0path"))
            .expect_err("NUL must not truncate a Windows path");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
