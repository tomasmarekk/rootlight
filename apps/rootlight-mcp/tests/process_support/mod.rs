//! Shared operating-system boundaries for real-process MCP tests.

#[cfg(unix)]
use std::fs;
use std::{thread, time::Duration};

use serde_json::Value;

#[allow(
    dead_code,
    reason = "each integration test compiles this shared module independently"
)]
const MAX_BUSY_ATTEMPTS: u8 = 3;

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

/// Replays a process-test setup call only when the public contract permits it.
#[allow(
    dead_code,
    reason = "not every integration test performs a setup tool call"
)]
pub(crate) fn retry_transient_busy(request_id: &str, mut call: impl FnMut(&str) -> Value) -> Value {
    for attempt in 1..=MAX_BUSY_ATTEMPTS {
        let response = call(&format!("{request_id}-attempt-{attempt}"));
        let error = &response["result"]["structuredContent"]["error"];
        let retryable_busy = error["code"] == "BUSY" && error["retryable"] == true;
        if !retryable_busy || attempt == MAX_BUSY_ATTEMPTS {
            return response;
        }

        // BUSY is the protocol's explicit replay signal after a saturated
        // daemon lane; all other responses return without being repeated.
        let retry_after_ms = error["retry_after_ms"]
            .as_u64()
            .unwrap_or(25)
            .clamp(1, 1_000);
        thread::sleep(Duration::from_millis(retry_after_ms));
    }
    unreachable!("bounded retry loop always returns")
}
