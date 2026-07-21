//! Rejects internal planning identifiers in Git metadata.
//!
//! Tracked content is covered by `source_hygiene`; this module extends the same
//! detection to commit messages and pull-request text so private planning and
//! PRD identifiers never enter Git metadata. Diagnostics report the offending
//! location and token class only, never the full private message body.

use std::{
    fs,
    path::Path,
    process::{Command, ExitStatus},
};

use crate::source_hygiene::{ForbiddenRule, forbidden_reference};

/// Git's scissors line: everything at and below it is removed from the message.
const SCISSORS_PREFIX: &str = "# ------------------------ >8 ------------------------";

pub(crate) fn check_commit_msg_file(path: &Path) -> Result<(), GitMetadataError> {
    let text = read_text(path, "commit message file")?;
    for (index, line) in text.lines().enumerate() {
        if line.starts_with(SCISSORS_PREFIX) {
            break;
        }
        if line.starts_with('#') {
            continue;
        }
        if let Some(rule) = forbidden_reference(line.as_bytes()) {
            return Err(GitMetadataError::CommitMessage {
                line: index.saturating_add(1),
                rule,
            });
        }
    }
    Ok(())
}

pub(crate) fn check_range(workspace_root: &Path, range: &str) -> Result<(), GitMetadataError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(["log", "--format=%H%x00%B%x00", range])
        .output()
        .map_err(GitMetadataError::GitIo)?;
    if !output.status.success() {
        return Err(GitMetadataError::GitFailed(output.status));
    }
    let mut fields = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|f| !f.is_empty());
    while let (Some(commit), Some(message)) = (fields.next(), fields.next()) {
        let commit = String::from_utf8_lossy(commit).into_owned();
        let message = String::from_utf8_lossy(message);
        if let Some(rule) = scan_message(&message) {
            return Err(GitMetadataError::RangeCommit { commit, rule });
        }
    }
    Ok(())
}

pub(crate) fn check_event(path: &Path) -> Result<(), GitMetadataError> {
    let text = read_text(path, "event payload")?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(GitMetadataError::EventParse)?;

    if let Some(pull_request) = value.get("pull_request") {
        for field in ["title", "body"] {
            if let Some(text) = pull_request.get(field).and_then(|item| item.as_str())
                && let Some(rule) = scan_message(text)
            {
                return Err(GitMetadataError::EventPayload {
                    location: format!("pull_request.{field}"),
                    rule,
                });
            }
        }
    }

    if let Some(commits) = value.get("commits").and_then(|item| item.as_array()) {
        for commit in commits {
            let id = commit
                .get("id")
                .and_then(|item| item.as_str())
                .unwrap_or("unknown");
            if let Some(message) = commit.get("message").and_then(|item| item.as_str())
                && let Some(rule) = scan_message(message)
            {
                return Err(GitMetadataError::EventPayload {
                    location: format!("commit {id}"),
                    rule,
                });
            }
        }
    }

    Ok(())
}

/// Returns the first forbidden rule found in a commit message, ignoring comment
/// lines and the scissors region exactly as Git does when finalizing a message.
fn scan_message(message: &str) -> Option<ForbiddenRule> {
    for line in message.lines() {
        if line.starts_with(SCISSORS_PREFIX) {
            break;
        }
        if line.starts_with('#') {
            continue;
        }
        if let Some(rule) = forbidden_reference(line.as_bytes()) {
            return Some(rule);
        }
    }
    None
}

fn read_text(path: &Path, what: &'static str) -> Result<String, GitMetadataError> {
    fs::read_to_string(path).map_err(|source| GitMetadataError::Read { what, source })
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum GitMetadataError {
    #[error("failed to read {what}")]
    Read {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("commit message contains {rule} on line {line}")]
    CommitMessage { line: usize, rule: ForbiddenRule },
    #[error("git log failed with status {0}")]
    GitFailed(ExitStatus),
    #[error("failed to run git log")]
    GitIo(#[source] std::io::Error),
    #[error("commit {commit} message contains {rule}")]
    RangeCommit { commit: String, rule: ForbiddenRule },
    #[error("event payload {location} contains {rule}")]
    EventPayload {
        location: String,
        rule: ForbiddenRule,
    },
    #[error("failed to parse event payload")]
    EventParse(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_conventional_message_passes() {
        let message =
            "fix(auth): handle expired tokens\n\nReturn a typed error instead of panicking.";
        assert_eq!(scan_message(message), None);
    }

    #[test]
    fn structured_label_in_subject_is_rejected() {
        let message = ["chore: complete TASK", "-10.2 cleanup"].concat();
        assert!(scan_message(&message).is_some());
    }

    #[test]
    fn numbered_plan_label_in_body_is_rejected() {
        let message = ["feat(mcp): serve queries\n\nImplements M", "15 pagination."].concat();
        assert!(scan_message(&message).is_some());
    }

    #[test]
    fn comment_lines_are_ignored() {
        // The label is assembled from parts so the forbidden token never appears
        // verbatim in tracked source, which the hygiene scan would reject.
        let label = ["TASK", "-99.1"].concat();
        let message = format!("fix: correct ordering\n# {label} appears only in a comment");
        assert_eq!(scan_message(&message), None);
    }

    #[test]
    fn scissors_region_is_ignored() {
        let label = ["TASK", "-99.1"].concat();
        let message = format!(
            "fix: correct ordering\n# ------------------------ >8 ------------------------\n{label} below scissors"
        );
        assert_eq!(scan_message(&message), None);
    }

    #[test]
    fn commit_msg_file_rejects_forbidden_line() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("COMMIT_EDITMSG");
        let content = ["feat: add surface\n\nCloses M", "16."].concat();
        fs::write(&path, content).expect("write");
        let error = check_commit_msg_file(&path).expect_err("forbidden label must fail");
        assert!(matches!(
            error,
            GitMetadataError::CommitMessage { line: 3, .. }
        ));
    }

    #[test]
    fn event_payload_pull_request_title_is_scanned() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("event.json");
        let payload = [
            "{\"pull_request\":{\"title\":\"Implement M",
            "15 tools\",\"body\":null}}",
        ]
        .concat();
        fs::write(&path, payload).expect("write");
        let error = check_event(&path).expect_err("forbidden label in title must fail");
        assert!(matches!(error, GitMetadataError::EventPayload { .. }));
    }

    #[test]
    fn event_payload_clean_push_passes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("event.json");
        let payload = r#"{"commits":[{"id":"abc123","message":"fix: correct ordering"}]}"#;
        fs::write(&path, payload).expect("write");
        assert!(check_event(&path).is_ok());
    }
}
