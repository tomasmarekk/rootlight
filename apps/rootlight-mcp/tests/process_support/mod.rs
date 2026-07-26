//! Shared operating-system boundaries for real-process MCP tests.

#[cfg(unix)]
use std::fs;

/// Creates a private fixture root with a portable authenticated-endpoint path.
pub(crate) fn private_process_tempdir(_prefix: &str) -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    let fixture = {
        // Keep authenticated Unix endpoints within macOS `sun_path`.
        tempfile::Builder::new()
            .prefix(_prefix)
            .tempdir_in("/private/tmp")
            .expect("isolated process fixture is available")
    };
    #[cfg(not(target_os = "macos"))]
    let fixture = tempfile::tempdir().expect("isolated process fixture is available");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700))
            .expect("process fixture permissions are private");
    }
    fixture
}
