//! Deterministic source-bound evidence for the currently safe operating mode.
//!
//! The report separates implemented bounded contracts from production
//! measurements and native controls that remain unavailable.

#![forbid(unsafe_code)]

use std::{
    ffi::{OsStr, OsString},
    io::{self, Write as _},
    process::ExitCode,
};

use serde::Serialize;

const CAPABILITY_EVIDENCE_SCHEMA: &str = "rootlight.capability-evidence/1";
const CAPABILITY_EVIDENCE_MAX_BYTES: usize = 32 * 1024;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1)) {
        Ok(encoded) => {
            let mut stdout = io::stdout().lock();
            if stdout
                .write_all(&encoded)
                .and_then(|()| stdout.write_all(b"\n"))
                .is_ok()
            {
                ExitCode::SUCCESS
            } else {
                eprintln!("error: capability evidence could not be written");
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<Vec<u8>, &'static str> {
    let source_revision = parse_arguments(arguments)?;
    encode_evidence(&source_revision)
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<String, &'static str> {
    let mut arguments = arguments.into_iter();
    let flag = next_argument(&mut arguments)?.ok_or("capability evidence arguments are invalid")?;
    if flag != OsStr::new("--source-revision") {
        return Err("capability evidence arguments are invalid");
    }
    let source_revision = next_argument(&mut arguments)?
        .and_then(|value| value.into_string().ok())
        .ok_or("capability evidence arguments are invalid")?;
    if next_argument(&mut arguments)?.is_some() || !is_source_revision(&source_revision) {
        return Err("capability evidence arguments are invalid");
    }
    Ok(source_revision)
}

fn next_argument<I>(arguments: &mut I) -> Result<Option<OsString>, &'static str>
where
    I: Iterator<Item = OsString>,
{
    let Some(argument) = arguments.next() else {
        return Ok(None);
    };
    if argument.as_encoded_bytes().len() > MAX_ARGUMENT_BYTES {
        return Err("capability evidence arguments are invalid");
    }
    Ok(Some(argument))
}

fn is_source_revision(source_revision: &str) -> bool {
    source_revision.len() == 40
        && source_revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn encode_evidence(source_revision: &str) -> Result<Vec<u8>, &'static str> {
    if !is_source_revision(source_revision) {
        return Err("capability evidence source revision is invalid");
    }
    let evidence = CapabilityEvidence {
        schema: CAPABILITY_EVIDENCE_SCHEMA,
        source_revision,
        decision: CapabilityDecision::Fallback,
        safe_operating_mode: SafeOperatingMode::ProcessLocalStructural,
        semantic_expansion_eligible: false,
        semantic: SemanticCapability {
            status: SemanticStatus::ContractFixtureOnly,
            declared_languages: ["go", "python", "rust", "typescript"],
            observed_language_reports: 0,
            holdout_available: false,
            language_breakdown_available: false,
            production_acceptance_eligible: false,
            tier_promotion_eligible: false,
        },
        incremental: IncrementalCapability {
            status: IncrementalStatus::ProcessLocalParserArtifactReuse,
            authoritative_reconcile_contract_available: true,
            parser_artifact_reuse_contract_available: true,
            fresh_generation_lowering_required: true,
            fixture_equivalence_ci_required: true,
            production_mutation_corpus_available: false,
            medium_suite_measurements_available: false,
            body_edit_p95_available: false,
            durable_artifact_cache_available: false,
        },
        storage: StorageCapability {
            selected_backend: StorageBackend::Sqlite,
            segment_status: SegmentStatus::InMemoryResearchOnly,
            verified_manifest_contract_available: true,
            recovery_classification_contract_available: true,
            lifecycle_contract_available: true,
            migration_contract_available: true,
            durable_filesystem_publication_active: false,
            restart_recovery_measurements_available: false,
            two_stage_publication_active: false,
        },
        isolation: IsolationCapability {
            activation: IsolationActivation::StructuralFallback,
            required_platforms: ["linux", "mac_os", "windows"],
            cross_platform_reports_required: true,
            native_controls_enforced: false,
            deep_adapter_permitted: false,
        },
        blocked_activations: BlockedActivations {
            language_tier_promotion: true,
            durable_publication: true,
            deep_adapter_execution: true,
            semantic_product_expansion: true,
        },
    };
    let encoded =
        serde_json::to_vec(&evidence).map_err(|_| "capability evidence encoding failed")?;
    if encoded.len() > CAPABILITY_EVIDENCE_MAX_BYTES {
        return Err("capability evidence exceeds its byte ceiling");
    }
    Ok(encoded)
}

#[derive(Debug, Serialize)]
struct CapabilityEvidence<'a> {
    schema: &'static str,
    source_revision: &'a str,
    decision: CapabilityDecision,
    safe_operating_mode: SafeOperatingMode,
    semantic_expansion_eligible: bool,
    semantic: SemanticCapability,
    incremental: IncrementalCapability,
    storage: StorageCapability,
    isolation: IsolationCapability,
    blocked_activations: BlockedActivations,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum CapabilityDecision {
    Fallback,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum SafeOperatingMode {
    ProcessLocalStructural,
}

#[derive(Debug, Serialize)]
struct SemanticCapability {
    status: SemanticStatus,
    declared_languages: [&'static str; 4],
    observed_language_reports: u8,
    holdout_available: bool,
    language_breakdown_available: bool,
    production_acceptance_eligible: bool,
    tier_promotion_eligible: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum SemanticStatus {
    ContractFixtureOnly,
}

#[derive(Debug, Serialize)]
struct IncrementalCapability {
    status: IncrementalStatus,
    authoritative_reconcile_contract_available: bool,
    parser_artifact_reuse_contract_available: bool,
    fresh_generation_lowering_required: bool,
    fixture_equivalence_ci_required: bool,
    production_mutation_corpus_available: bool,
    medium_suite_measurements_available: bool,
    body_edit_p95_available: bool,
    durable_artifact_cache_available: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum IncrementalStatus {
    ProcessLocalParserArtifactReuse,
}

#[derive(Debug, Serialize)]
struct StorageCapability {
    selected_backend: StorageBackend,
    segment_status: SegmentStatus,
    verified_manifest_contract_available: bool,
    recovery_classification_contract_available: bool,
    lifecycle_contract_available: bool,
    migration_contract_available: bool,
    durable_filesystem_publication_active: bool,
    restart_recovery_measurements_available: bool,
    two_stage_publication_active: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum StorageBackend {
    Sqlite,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum SegmentStatus {
    InMemoryResearchOnly,
}

#[derive(Debug, Serialize)]
struct IsolationCapability {
    activation: IsolationActivation,
    required_platforms: [&'static str; 3],
    cross_platform_reports_required: bool,
    native_controls_enforced: bool,
    deep_adapter_permitted: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum IsolationActivation {
    StructuralFallback,
}

#[derive(Debug, Serialize)]
struct BlockedActivations {
    language_tier_promotion: bool,
    durable_publication: bool,
    deep_adapter_execution: bool,
    semantic_product_expansion: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn report_is_deterministic_bounded_and_source_bound() {
        let first = encode_evidence(REVISION).expect("capability evidence encodes");
        let second = encode_evidence(REVISION).expect("capability evidence re-encodes");
        assert_eq!(first, second);
        assert!(first.len() <= CAPABILITY_EVIDENCE_MAX_BYTES);

        let value: serde_json::Value =
            serde_json::from_slice(&first).expect("capability evidence decodes");
        assert_eq!(value["schema"], CAPABILITY_EVIDENCE_SCHEMA);
        assert_eq!(value["source_revision"], REVISION);
        assert_eq!(value["decision"], "fallback");
        assert_eq!(value["semantic_expansion_eligible"], false);
    }

    #[test]
    fn report_cannot_activate_unmeasured_or_unenforced_paths() {
        let encoded = encode_evidence(REVISION).expect("capability evidence encodes");
        let value: serde_json::Value =
            serde_json::from_slice(&encoded).expect("capability evidence decodes");
        assert_eq!(value["semantic"]["production_acceptance_eligible"], false);
        assert_eq!(
            value["incremental"]["medium_suite_measurements_available"],
            false
        );
        assert_eq!(
            value["storage"]["durable_filesystem_publication_active"],
            false
        );
        assert_eq!(value["isolation"]["deep_adapter_permitted"], false);
        assert!(
            value["blocked_activations"]
                .as_object()
                .expect("blocked activations are an object")
                .values()
                .all(|blocked| blocked == true)
        );
    }

    #[test]
    fn arguments_reject_noncanonical_and_oversized_values() {
        assert!(
            run(["--source-revision", REVISION]
                .into_iter()
                .map(OsString::from))
            .is_ok()
        );
        assert!(run(std::iter::empty()).is_err());
        assert!(
            run(["--source-revision", &REVISION.to_ascii_uppercase()]
                .into_iter()
                .map(OsString::from))
            .is_err()
        );
        assert!(
            run([
                OsString::from("--source-revision"),
                OsString::from("x".repeat(MAX_ARGUMENT_BYTES + 1)),
            ])
            .is_err()
        );
    }
}
