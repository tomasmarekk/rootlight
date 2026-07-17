//! Explicit bounded decoders for externally supplied benchmark JSON.
//!
//! Raw external inputs cross this module's byte, collection, string, digest,
//! path, and aggregate-size checks before becoming trusted model values.

use std::collections::BTreeSet;

use serde::Deserialize;

use crate::{BenchmarkCommand, BundleLimits, DatasetEntry, DatasetManifest, bundle::BundleError};

/// Decodes a strict dataset manifest under checked limits.
///
/// # Errors
///
/// Returns [`DecodeError`] for an invalid limit set, oversized input, malformed
/// JSON, unknown fields, non-canonical identifiers, digests or paths, and
/// collection or declared-byte limit violations.
pub fn decode_dataset_manifest(
    bytes: &[u8],
    limits: BundleLimits,
) -> Result<DatasetManifest, DecodeError> {
    let limits = limits.validate().map_err(DecodeError::Limits)?;
    check_input_bytes(bytes, limits)?;
    let input: DatasetManifestInput =
        serde_json::from_slice(bytes).map_err(|_| DecodeError::InvalidJson)?;
    validate_string(&input.schema_version, limits, StringKind::Label)?;
    if input.schema_version != "1.0" {
        return Err(DecodeError::InvalidSchema);
    }
    validate_string(&input.dataset_id, limits, StringKind::Label)?;
    validate_string(&input.revision, limits, StringKind::Text)?;
    validate_string(&input.scope_rule, limits, StringKind::Label)?;
    validate_string(&input.loc_counting_rule, limits, StringKind::Label)?;
    if input.entries.len() > limits.max_manifest_entries {
        return Err(DecodeError::LimitExceeded {
            resource: "manifest_entry_count",
        });
    }

    let mut total_source_bytes = 0_u64;
    let mut prior_id: Option<&str> = None;
    let mut paths = BTreeSet::new();
    for entry in &input.entries {
        validate_string(&entry.id, limits, StringKind::Label)?;
        validate_string(&entry.grammar_family, limits, StringKind::Label)?;
        validate_string(&entry.language, limits, StringKind::Label)?;
        validate_relative_path(&entry.relative_path, limits)?;
        validate_digest(&entry.source_sha256)?;
        if prior_id.is_some_and(|prior| entry.id.as_str() <= prior) {
            return Err(DecodeError::NonCanonicalOrder);
        }
        prior_id = Some(&entry.id);
        if !paths.insert(entry.relative_path.as_str()) {
            return Err(DecodeError::DuplicatePath);
        }
        if entry.source_bytes > limits.max_snapshot_bytes {
            return Err(DecodeError::LimitExceeded {
                resource: "snapshot_bytes",
            });
        }
        total_source_bytes = total_source_bytes.checked_add(entry.source_bytes).ok_or(
            DecodeError::LimitExceeded {
                resource: "dataset_source_bytes",
            },
        )?;
        if total_source_bytes > limits.max_dataset_source_bytes {
            return Err(DecodeError::LimitExceeded {
                resource: "dataset_source_bytes",
            });
        }
        let maximum_lines =
            entry
                .source_bytes
                .checked_add(1)
                .ok_or(DecodeError::LimitExceeded {
                    resource: "snapshot_bytes",
                })?;
        if entry.physical_lines > maximum_lines {
            return Err(DecodeError::InvalidPhysicalLineCount);
        }
    }

    Ok(DatasetManifest {
        schema_version: input.schema_version,
        dataset_id: input.dataset_id,
        revision: input.revision,
        scope_rule: input.scope_rule,
        loc_counting_rule: input.loc_counting_rule,
        entries: input
            .entries
            .into_iter()
            .map(|entry| DatasetEntry {
                id: entry.id,
                grammar_family: entry.grammar_family,
                language: entry.language,
                relative_path: entry.relative_path,
                source_sha256: entry.source_sha256,
                source_bytes: entry.source_bytes,
                physical_lines: entry.physical_lines,
                generated: entry.generated,
            })
            .collect(),
    })
}

/// Decodes a strict normalized command document under checked limits.
///
/// # Errors
///
/// Returns [`DecodeError`] for an invalid limit set, oversized input, malformed
/// JSON, unknown fields, path-shaped arguments, or invalid run counts.
pub fn decode_benchmark_command(
    bytes: &[u8],
    limits: BundleLimits,
) -> Result<BenchmarkCommand, DecodeError> {
    let limits = limits.validate().map_err(DecodeError::Limits)?;
    check_input_bytes(bytes, limits)?;
    let input: BenchmarkCommandInput =
        serde_json::from_slice(bytes).map_err(|_| DecodeError::InvalidJson)?;
    validate_string(&input.schema_version, limits, StringKind::Label)?;
    if input.schema_version != "1.0" {
        return Err(DecodeError::InvalidSchema);
    }
    validate_string(&input.subcommand, limits, StringKind::Label)?;
    if input.arguments.len() > limits.max_command_arguments {
        return Err(DecodeError::LimitExceeded {
            resource: "command_argument_count",
        });
    }
    for argument in &input.arguments {
        validate_string(argument, limits, StringKind::Text)?;
        if is_path_shaped(argument) {
            return Err(DecodeError::PathShapedValue);
        }
    }
    if input.warmup_rounds == 0 || input.trial_rounds == 0 || input.timeout_ms == 0 {
        return Err(DecodeError::InvalidRunPolicy);
    }
    Ok(BenchmarkCommand {
        schema_version: input.schema_version,
        subcommand: input.subcommand,
        arguments: input.arguments,
        seed: input.seed,
        warmup_rounds: input.warmup_rounds,
        trial_rounds: input.trial_rounds,
        timeout_ms: input.timeout_ms,
    })
}

fn check_input_bytes(bytes: &[u8], limits: BundleLimits) -> Result<(), DecodeError> {
    if bytes.is_empty() || bytes.len() > limits.max_input_bytes {
        return Err(DecodeError::LimitExceeded {
            resource: "input_bytes",
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum StringKind {
    Label,
    Text,
}

fn validate_string(value: &str, limits: BundleLimits, kind: StringKind) -> Result<(), DecodeError> {
    if value.is_empty()
        || value.len() > limits.max_string_bytes
        || value.chars().any(char::is_control)
    {
        return Err(DecodeError::InvalidString);
    }
    if matches!(kind, StringKind::Label)
        && !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        return Err(DecodeError::InvalidString);
    }
    Ok(())
}

fn validate_relative_path(value: &str, limits: BundleLimits) -> Result<(), DecodeError> {
    validate_string(value, limits, StringKind::Text)?;
    if value.starts_with('/')
        || value.starts_with('\\')
        || value.contains('\\')
        || value.contains("//")
        || value.split('/').any(|component| {
            component.is_empty() || matches!(component, "." | "..") || component.ends_with('.')
        })
        || value.as_bytes().get(1).is_some_and(|byte| *byte == b':')
    {
        return Err(DecodeError::InvalidRelativePath);
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), DecodeError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(DecodeError::InvalidDigest);
    }
    Ok(())
}

fn is_path_shaped(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with('\\')
        || value.starts_with("~/")
        || value.contains('\\')
        || value.contains("://")
        || value.contains("../")
        || value.as_bytes().get(1).is_some_and(|byte| *byte == b':')
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatasetManifestInput {
    schema_version: String,
    dataset_id: String,
    revision: String,
    scope_rule: String,
    loc_counting_rule: String,
    entries: Vec<DatasetEntryInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatasetEntryInput {
    id: String,
    grammar_family: String,
    language: String,
    relative_path: String,
    source_sha256: String,
    source_bytes: u64,
    physical_lines: u64,
    generated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkCommandInput {
    schema_version: String,
    subcommand: String,
    arguments: Vec<String>,
    seed: u64,
    warmup_rounds: u32,
    trial_rounds: u32,
    timeout_ms: u64,
}

/// Strict benchmark-input decoding failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The supplied bundle/decode limits are invalid.
    #[error("benchmark decode limits are invalid")]
    Limits(#[source] BundleError),
    /// A bounded input resource exceeded its ceiling.
    #[error("benchmark input limit exceeded: {resource}")]
    LimitExceeded {
        /// Stable source-free resource label.
        resource: &'static str,
    },
    /// The input is not strict JSON for the expected document shape.
    #[error("benchmark input JSON is invalid")]
    InvalidJson,
    /// The schema version is unsupported.
    #[error("benchmark input schema is invalid")]
    InvalidSchema,
    /// A string is empty, oversized, contains controls, or is not canonical.
    #[error("benchmark input string is invalid")]
    InvalidString,
    /// A manifest digest is not canonical lowercase SHA-256.
    #[error("benchmark input digest is invalid")]
    InvalidDigest,
    /// A manifest source path is not canonical repository-relative syntax.
    #[error("benchmark input relative path is invalid")]
    InvalidRelativePath,
    /// Manifest entries are not in strictly increasing ID order.
    #[error("benchmark manifest order is invalid")]
    NonCanonicalOrder,
    /// Two manifest entries declare the same repository-relative path.
    #[error("benchmark manifest path is duplicated")]
    DuplicatePath,
    /// A physical-line count cannot fit the declared source bytes.
    #[error("benchmark manifest physical line count is invalid")]
    InvalidPhysicalLineCount,
    /// A normalized argument resembles a host path or URL.
    #[error("benchmark command contains a path-shaped value")]
    PathShapedValue,
    /// Warm-up, trial, or timeout policy is zero.
    #[error("benchmark command run policy is invalid")]
    InvalidRunPolicy,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(digest: &str, path: &str) -> Vec<u8> {
        format!(
            r#"{{
                "schema_version":"1.0",
                "dataset_id":"fixture",
                "revision":"rev-1",
                "scope_rule":"listed_entries",
                "loc_counting_rule":"physical_newlines",
                "entries":[{{
                    "id":"entry-1",
                    "grammar_family":"rust",
                    "language":"rust",
                    "relative_path":"{path}",
                    "source_sha256":"{digest}",
                    "source_bytes":4,
                    "physical_lines":1,
                    "generated":false
                }}]
            }}"#
        )
        .into_bytes()
    }

    #[test]
    fn manifest_decoder_accepts_only_canonical_lowercase_digests() {
        let lowercase = "ab".repeat(32);
        decode_dataset_manifest(&manifest(&lowercase, "src/lib.rs"), BundleLimits::default())
            .expect("canonical manifest decodes");

        let uppercase = "AB".repeat(32);
        let error =
            decode_dataset_manifest(&manifest(&uppercase, "src/lib.rs"), BundleLimits::default())
                .expect_err("uppercase digest is rejected");
        assert!(matches!(error, DecodeError::InvalidDigest));
    }

    #[test]
    fn manifest_decoder_bounds_input_entries_snapshot_and_total_bytes() {
        let digest = "ab".repeat(32);
        let bytes = manifest(&digest, "src/lib.rs");
        let limits = BundleLimits {
            max_input_bytes: bytes.len() - 1,
            ..BundleLimits::default()
        };
        assert!(matches!(
            decode_dataset_manifest(&bytes, limits),
            Err(DecodeError::LimitExceeded {
                resource: "input_bytes"
            })
        ));

        let limits = BundleLimits {
            max_manifest_entries: 1,
            ..BundleLimits::default()
        };
        let doubled = String::from_utf8(bytes)
            .expect("fixture is UTF-8")
            .replace("}]\n", "},{\"id\":\"entry-2\",\"grammar_family\":\"rust\",\"language\":\"rust\",\"relative_path\":\"src/main.rs\",\"source_sha256\":\"abababababababababababababababababababababababababababababababab\",\"source_bytes\":4,\"physical_lines\":1,\"generated\":false}]\n");
        assert!(matches!(
            decode_dataset_manifest(doubled.as_bytes(), limits),
            Err(DecodeError::LimitExceeded {
                resource: "manifest_entry_count"
            })
        ));

        let limits = BundleLimits {
            max_snapshot_bytes: 3,
            ..BundleLimits::default()
        };
        assert!(matches!(
            decode_dataset_manifest(&manifest(&digest, "src/lib.rs"), limits),
            Err(DecodeError::LimitExceeded {
                resource: "snapshot_bytes"
            })
        ));

        let limits = BundleLimits {
            max_dataset_source_bytes: 3,
            ..BundleLimits::default()
        };
        assert!(matches!(
            decode_dataset_manifest(&manifest(&digest, "src/lib.rs"), limits),
            Err(DecodeError::LimitExceeded {
                resource: "dataset_source_bytes"
            })
        ));
    }

    #[test]
    fn manifest_decoder_rejects_unknown_fields_and_noncanonical_paths() {
        let digest = "ab".repeat(32);
        let unknown = String::from_utf8(manifest(&digest, "src/lib.rs"))
            .expect("fixture is UTF-8")
            .replace("\"generated\":false", "\"generated\":false,\"extra\":true");
        assert!(matches!(
            decode_dataset_manifest(unknown.as_bytes(), BundleLimits::default()),
            Err(DecodeError::InvalidJson)
        ));

        assert!(matches!(
            decode_dataset_manifest(&manifest(&digest, "../outside.rs"), BundleLimits::default()),
            Err(DecodeError::InvalidRelativePath)
        ));
    }

    #[test]
    fn command_decoder_is_strict_bounded_and_source_free() {
        let valid = br#"{
            "schema_version":"1.0",
            "subcommand":"m05-parser",
            "arguments":["dataset=fixture"],
            "seed":7,
            "warmup_rounds":1,
            "trial_rounds":2,
            "timeout_ms":1000
        }"#;
        decode_benchmark_command(valid, BundleLimits::default())
            .expect("bounded source-free command decodes");

        let path_shaped = br#"{
            "schema_version":"1.0",
            "subcommand":"m05-parser",
            "arguments":["C:/source/repo"],
            "seed":7,
            "warmup_rounds":1,
            "trial_rounds":2,
            "timeout_ms":1000
        }"#;
        assert!(matches!(
            decode_benchmark_command(path_shaped, BundleLimits::default()),
            Err(DecodeError::PathShapedValue)
        ));

        let unknown = String::from_utf8(valid.to_vec())
            .expect("fixture is UTF-8")
            .replace("\"timeout_ms\":1000", "\"timeout_ms\":1000,\"extra\":true");
        assert!(matches!(
            decode_benchmark_command(unknown.as_bytes(), BundleLimits::default()),
            Err(DecodeError::InvalidJson)
        ));
    }
}
