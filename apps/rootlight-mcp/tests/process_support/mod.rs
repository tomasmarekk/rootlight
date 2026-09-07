//! Shared operating-system boundaries for real-process MCP tests.

#[cfg(unix)]
use std::fs;
use std::{
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

/// Verifies one exact symbol followed by distinct source-only evidence for its file.
#[allow(dead_code, reason = "not every process test performs a locate query")]
pub(crate) fn assert_symbol_and_source_matches(matches: &[Value]) {
    assert_eq!(matches.len(), 2);
    assert!(matches[0]["symbol_id"].is_string());
    assert!(matches[1]["symbol_id"].is_null());
    assert_eq!(matches[1]["kind"], "file");
    assert_eq!(matches[1]["file_id"], matches[0]["file_id"]);
    assert_eq!(matches[1]["path"], matches[0]["path"]);
    assert!(matches[0]["source_ref"].is_object());
    assert!(matches[1]["source_ref"].is_object());
    for field in ["repository", "generation", "content_hash"] {
        assert_eq!(
            matches[1]["source_ref"][field],
            matches[0]["source_ref"][field]
        );
    }
}

#[allow(
    dead_code,
    reason = "each integration test compiles this shared module independently"
)]
const MAX_BUSY_ATTEMPTS: u16 = 256;
#[allow(
    dead_code,
    reason = "each integration test compiles this shared module independently"
)]
const MAX_BUSY_WAIT: Duration = Duration::from_secs(30);

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
    let deadline = Instant::now()
        .checked_add(MAX_BUSY_WAIT)
        .expect("bounded process-test deadline is representable");
    for attempt in 1..=MAX_BUSY_ATTEMPTS {
        let response = call(&format!("{request_id}-attempt-{attempt}"));
        let error = &response["result"]["structuredContent"]["error"];
        let retryable_busy = error["code"] == "BUSY" && error["retryable"] == true;
        let now = Instant::now();
        if !retryable_busy || attempt == MAX_BUSY_ATTEMPTS || now >= deadline {
            return response;
        }

        // BUSY is the protocol's explicit replay signal after a saturated
        // daemon lane; all other responses return without being repeated.
        let retry_after_ms = error["retry_after_ms"]
            .as_u64()
            .unwrap_or(25)
            .clamp(1, 1_000);
        thread::sleep(
            Duration::from_millis(retry_after_ms).min(deadline.saturating_duration_since(now)),
        );
    }
    unreachable!("bounded retry loop always returns")
}
