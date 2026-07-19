//! Rejects internal planning identifiers in tracked repository content.
//!
//! Public source names describe product behavior rather than private execution
//! order, so the same check covers paths and text on every supported platform.

use std::{
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

const MAX_TRACKED_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_TRACKED_PATHS: usize = 100_000;
const MAX_TRACKED_FILE_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) fn check(workspace_root: &Path) -> Result<(), SourceHygieneError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(SourceHygieneError::ListTrackedFiles)?;
    if !output.status.success() {
        return Err(SourceHygieneError::GitFailed(output.status));
    }
    if output.stdout.len() > MAX_TRACKED_PATH_BYTES {
        return Err(SourceHygieneError::LimitExceeded("tracked_path_bytes"));
    }

    let mut path_count = 0_usize;
    for encoded_path in output.stdout.split(|byte| *byte == 0) {
        if encoded_path.is_empty() {
            continue;
        }
        path_count = path_count
            .checked_add(1)
            .ok_or(SourceHygieneError::LimitExceeded("tracked_path_count"))?;
        if path_count > MAX_TRACKED_PATHS {
            return Err(SourceHygieneError::LimitExceeded("tracked_path_count"));
        }

        let relative_path = std::str::from_utf8(encoded_path)
            .map_err(|_| SourceHygieneError::NonUtf8Path)?
            .replace('\\', "/");
        if relative_path == ".gitignore" {
            continue;
        }
        if is_internal_support_path(&relative_path) {
            return Err(SourceHygieneError::ForbiddenReference {
                path: PathBuf::from(&relative_path),
                line: None,
                rule: ForbiddenRule::InternalSupportDocument,
            });
        }
        if let Some(rule) = forbidden_reference(relative_path.as_bytes()) {
            return Err(SourceHygieneError::ForbiddenReference {
                path: PathBuf::from(&relative_path),
                line: None,
                rule,
            });
        }

        check_file(workspace_root, &relative_path)?;
    }
    Ok(())
}

fn check_file(workspace_root: &Path, relative_path: &str) -> Result<(), SourceHygieneError> {
    let path = workspace_root.join(relative_path);
    let byte_length = fs::metadata(&path)
        .map_err(|source| SourceHygieneError::Read {
            path: path.clone(),
            source,
        })?
        .len();
    if byte_length > MAX_TRACKED_FILE_BYTES {
        return Err(SourceHygieneError::OversizedFile { path, byte_length });
    }

    let read_limit = MAX_TRACKED_FILE_BYTES
        .checked_add(1)
        .ok_or(SourceHygieneError::LimitExceeded("tracked_file_bytes"))?;
    let capacity = usize::try_from(byte_length)
        .map_err(|_| SourceHygieneError::LimitExceeded("tracked_file_bytes"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| SourceHygieneError::AllocationFailed)?;
    File::open(&path)
        .map_err(|source| SourceHygieneError::Read {
            path: path.clone(),
            source,
        })?
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| SourceHygieneError::Read {
            path: path.clone(),
            source,
        })?;
    let observed_byte_length = u64::try_from(bytes.len())
        .map_err(|_| SourceHygieneError::LimitExceeded("tracked_file_bytes"))?;
    if observed_byte_length > MAX_TRACKED_FILE_BYTES {
        return Err(SourceHygieneError::OversizedFile {
            path,
            byte_length: observed_byte_length,
        });
    }
    if bytes.contains(&0) {
        return Ok(());
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(());
    };

    for (line_index, line) in text.lines().enumerate() {
        let rule = forbidden_reference(line.as_bytes()).or_else(|| {
            line.chars()
                .any(is_czech_specific_letter)
                .then_some(ForbiddenRule::NonEnglishText)
        });
        if let Some(rule) = rule {
            let line = line_index
                .checked_add(1)
                .ok_or(SourceHygieneError::LimitExceeded("line_number"))?;
            return Err(SourceHygieneError::ForbiddenReference {
                path,
                line: Some(line),
                rule,
            });
        }
    }
    Ok(())
}

const fn is_czech_specific_letter(character: char) -> bool {
    matches!(
        character,
        '\u{010c}'
            | '\u{010d}'
            | '\u{010e}'
            | '\u{010f}'
            | '\u{011a}'
            | '\u{011b}'
            | '\u{0147}'
            | '\u{0148}'
            | '\u{0158}'
            | '\u{0159}'
            | '\u{0160}'
            | '\u{0161}'
            | '\u{0164}'
            | '\u{0165}'
            | '\u{016e}'
            | '\u{016f}'
            | '\u{017d}'
            | '\u{017e}'
    )
}

fn forbidden_reference(input: &[u8]) -> Option<ForbiddenRule> {
    if contains_numbered_plan_label(input) {
        return Some(ForbiddenRule::NumberedPlanLabel);
    }
    if contains_joined(input, b"mile", b"stone") || contains_joined(input, b"road", b"map") {
        return Some(ForbiddenRule::PlanningTerm);
    }
    if contains_numbered_gate_label(input) {
        return Some(ForbiddenRule::StructuredPlanLabel);
    }
    if contains_numbered_verification_label(input) {
        return Some(ForbiddenRule::StructuredPlanLabel);
    }
    for (head, tail) in [
        (b"TASK".as_slice(), b"-".as_slice()),
        (b"REQ".as_slice(), b"-".as_slice()),
        (b"CMP".as_slice(), b"-".as_slice()),
        (b"EPIC".as_slice(), b"-".as_slice()),
        (b"PG".as_slice(), b"-".as_slice()),
        (b"ADR".as_slice(), b"-".as_slice()),
        (b"BENCH".as_slice(), b"-".as_slice()),
        (b"FUZZ".as_slice(), b"-".as_slice()),
        (b"TEST".as_slice(), b"-".as_slice()),
        (b"SEC".as_slice(), b"-".as_slice()),
    ] {
        if contains_joined_case_sensitive(input, head, tail) {
            return Some(ForbiddenRule::StructuredPlanLabel);
        }
    }
    None
}

fn contains_numbered_verification_label(input: &[u8]) -> bool {
    [b"bench".as_slice(), b"fuzz", b"test", b"sec"]
        .into_iter()
        .any(|head| {
            input
                .windows(head.len() + 1)
                .enumerate()
                .any(|(index, window)| {
                    if (index != 0 && is_identifier_byte(input[index - 1]))
                        || !window[..head.len()].eq_ignore_ascii_case(head)
                        || !matches!(window[head.len()], b'-' | b'_')
                    {
                        return false;
                    }
                    let tail = &input[index + head.len() + 1..];
                    let token_length = tail
                        .iter()
                        .position(|byte| !is_identifier_byte(*byte) && *byte != b'-')
                        .unwrap_or(tail.len());
                    tail[..token_length].windows(4).any(|number| {
                        matches!(number[0], b'-' | b'_')
                            && number[1..].iter().all(u8::is_ascii_digit)
                    })
                })
        })
}

fn is_internal_support_path(relative_path: &str) -> bool {
    let components = relative_path.split('/').collect::<Vec<_>>();
    components.windows(2).any(|pair| {
        (pair[0].eq_ignore_ascii_case("policy") && pair[1].eq_ignore_ascii_case("adr"))
            || (pair[0].eq_ignore_ascii_case("docs") && pair[1].eq_ignore_ascii_case("execution"))
    }) || components
        .iter()
        .any(|component| component.eq_ignore_ascii_case("development-docs"))
}

fn contains_numbered_plan_label(input: &[u8]) -> bool {
    input.windows(3).enumerate().any(|(index, window)| {
        window[0].eq_ignore_ascii_case(&b'm')
            && window[1].is_ascii_digit()
            && window[2].is_ascii_digit()
            && (index == 0 || !is_identifier_byte(input[index - 1]))
    })
}

fn contains_numbered_gate_label(input: &[u8]) -> bool {
    const HEAD: &[u8] = b"gate";
    input
        .windows(HEAD.len())
        .enumerate()
        .any(|(index, window)| {
            if (index != 0 && is_identifier_byte(input[index - 1]))
                || !window.eq_ignore_ascii_case(HEAD)
            {
                return false;
            }
            let Some(separator) = input.get(index + HEAD.len()) else {
                return false;
            };
            if !matches!(separator, b'-' | b'_') {
                return false;
            }
            let tail = &input[index + HEAD.len() + 1..];
            let tail = tail
                .strip_prefix(b"v")
                .or_else(|| tail.strip_prefix(b"V"))
                .unwrap_or(tail);
            tail.first().is_some_and(u8::is_ascii_digit)
        })
}

fn contains_joined(input: &[u8], head: &[u8], tail: &[u8]) -> bool {
    let pattern_length = match head.len().checked_add(tail.len()) {
        Some(length) => length,
        None => return false,
    };
    if pattern_length == 0 || input.len() < pattern_length {
        return false;
    }

    input
        .windows(pattern_length)
        .enumerate()
        .any(|(index, window)| {
            (index == 0 || !is_identifier_byte(input[index - 1]))
                && window[..head.len()].eq_ignore_ascii_case(head)
                && window[head.len()..].eq_ignore_ascii_case(tail)
        })
}

fn contains_joined_case_sensitive(input: &[u8], head: &[u8], tail: &[u8]) -> bool {
    let pattern_length = match head.len().checked_add(tail.len()) {
        Some(length) => length,
        None => return false,
    };
    if pattern_length == 0 || input.len() < pattern_length {
        return false;
    }

    input
        .windows(pattern_length)
        .enumerate()
        .any(|(index, window)| {
            (index == 0 || !is_identifier_byte(input[index - 1]))
                && window[..head.len()] == *head
                && window[head.len()..] == *tail
        })
}

const fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForbiddenRule {
    NumberedPlanLabel,
    PlanningTerm,
    StructuredPlanLabel,
    NonEnglishText,
    InternalSupportDocument,
}

impl std::fmt::Display for ForbiddenRule {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::NumberedPlanLabel => "numbered internal plan label",
            Self::PlanningTerm => "internal planning terminology",
            Self::StructuredPlanLabel => "structured internal plan label",
            Self::NonEnglishText => "non-English prose marker",
            Self::InternalSupportDocument => "internal support document path",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SourceHygieneError {
    #[error("failed to list tracked files")]
    ListTrackedFiles(#[source] std::io::Error),
    #[error("git ls-files failed with status {0}")]
    GitFailed(ExitStatus),
    #[error("tracked repository contains a non-UTF-8 path")]
    NonUtf8Path,
    #[error("source hygiene limit exceeded: {0}")]
    LimitExceeded(&'static str),
    #[error("source hygiene allocation failed")]
    AllocationFailed,
    #[error("failed to read tracked file {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "tracked file {} contains {byte_length} bytes and exceeds the {} byte hygiene ceiling",
        path.display(),
        MAX_TRACKED_FILE_BYTES
    )]
    OversizedFile { path: PathBuf, byte_length: u64 },
    #[error(
        "tracked source contains {rule} at {}{}",
        path.display(),
        line.map(|number| format!(":{number}")).unwrap_or_default()
    )]
    ForbiddenReference {
        path: PathBuf,
        line: Option<usize>,
        rule: ForbiddenRule,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_plan_labels_are_rejected_without_false_positives() {
        let label = ["M", "09", "SemanticEvidence"].concat();
        assert_eq!(
            forbidden_reference(label.as_bytes()),
            Some(ForbiddenRule::NumberedPlanLabel)
        );
        assert_eq!(forbidden_reference(b"bm25 arm64"), None);
    }

    #[test]
    fn structured_plan_labels_are_rejected() {
        let label = ["TASK", "-09.1"].concat();
        assert_eq!(
            forbidden_reference(label.as_bytes()),
            Some(ForbiddenRule::StructuredPlanLabel)
        );
        let numbered_gate = ["gate", "_v2"].concat();
        assert_eq!(
            forbidden_reference(numbered_gate.as_bytes()),
            Some(ForbiddenRule::StructuredPlanLabel)
        );
    }

    #[test]
    fn planning_terms_are_rejected() {
        let label = ["road", "map"].concat();
        assert_eq!(
            forbidden_reference(label.as_bytes()),
            Some(ForbiddenRule::PlanningTerm)
        );
    }

    #[test]
    fn czech_specific_letters_are_rejected() {
        assert!(is_czech_specific_letter('\u{0159}'));
        assert!(!is_czech_specific_letter('\u{00e9}'));
    }

    #[test]
    fn internal_support_paths_are_rejected() {
        let path = ["policy", "adr", "note.md"].join("/");
        assert!(is_internal_support_path(&path));
        assert!(!is_internal_support_path("README.md"));
    }

    #[test]
    fn numbered_verification_labels_are_rejected() {
        let label = ["fuzz", "_parser", "_001"].concat();
        assert!(contains_numbered_verification_label(label.as_bytes()));
        assert!(!contains_numbered_verification_label(b"test_fixture"));
    }
}
