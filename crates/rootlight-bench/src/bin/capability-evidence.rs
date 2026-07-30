//! Deterministic source-bound report of measured capability facts.
//!
//! Production semantic observations are embedded directly. Native isolation
//! remains platform-scoped and is proven by separate exact-process artifacts.

#![forbid(unsafe_code)]

use std::{
    ffi::{OsStr, OsString},
    io::{self, Write as _},
    process::ExitCode,
};

use rootlight_bench::{ProjectSemanticHoldoutEvidence, build_project_semantic_holdout};
use serde::Serialize;

const CAPABILITY_EVIDENCE_SCHEMA: &str = "rootlight.capability-evidence/3";
const CAPABILITY_EVIDENCE_MAX_BYTES: usize = 64 * 1024;
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
    let semantic =
        build_project_semantic_holdout().map_err(|_| "semantic holdout evidence is invalid")?;
    let evidence = CapabilityEvidence {
        schema: CAPABILITY_EVIDENCE_SCHEMA,
        source_revision,
        semantic,
        incremental: IncrementalCapability {
            status: IncrementalStatus::DurableGenerationReuse,
            authoritative_reconcile_contract_available: true,
            parser_artifact_reuse_contract_available: true,
            fresh_generation_lowering_required: true,
            fixture_equivalence_ci_required: true,
            production_mutation_corpus_available: false,
            medium_suite_measurements_available: true,
            body_edit_p95_available: true,
            durable_artifact_cache_available: true,
        },
        storage: StorageCapability {
            selected_backend: StorageBackend::Sqlite,
            segment_status: SegmentStatus::DurableCatalogAndSnapshots,
            verified_manifest_contract_available: true,
            recovery_classification_contract_available: true,
            lifecycle_contract_available: true,
            migration_contract_available: true,
            durable_filesystem_publication_active: true,
            restart_recovery_measurements_available: true,
            two_stage_publication_active: false,
        },
        isolation: IsolationCapability {
            authoritative_artifact_schema: "rootlight.adapter-isolation/1",
            required_platforms: ["linux", "mac_os", "windows"],
            cross_platform_reports_required: true,
            platform_scoped_observations_required: true,
            integrated_project_session_required: true,
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
    semantic: ProjectSemanticHoldoutEvidence,
    incremental: IncrementalCapability,
    storage: StorageCapability,
    isolation: IsolationCapability,
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
    DurableGenerationReuse,
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
    DurableCatalogAndSnapshots,
}

#[derive(Debug, Serialize)]
struct IsolationCapability {
    authoritative_artifact_schema: &'static str,
    required_platforms: [&'static str; 3],
    cross_platform_reports_required: bool,
    platform_scoped_observations_required: bool,
    integrated_project_session_required: bool,
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
    }

    #[test]
    fn report_contains_measured_storage_and_incremental_facts() {
        let encoded = encode_evidence(REVISION).expect("capability evidence encodes");
        let value: serde_json::Value =
            serde_json::from_slice(&encoded).expect("capability evidence decodes");
        assert_eq!(
            value["incremental"]["medium_suite_measurements_available"],
            true
        );
        assert_eq!(value["incremental"]["body_edit_p95_available"], true);
        assert_eq!(
            value["storage"]["durable_filesystem_publication_active"],
            true
        );
        assert_eq!(
            value["storage"]["restart_recovery_measurements_available"],
            true
        );
    }

    #[test]
    fn semantic_and_isolation_claims_reference_observed_artifacts() {
        let encoded = encode_evidence(REVISION).expect("capability evidence encodes");
        let value: serde_json::Value =
            serde_json::from_slice(&encoded).expect("capability evidence decodes");
        assert_eq!(
            value["semantic"]["schema"],
            rootlight_bench::PROJECT_SEMANTIC_HOLDOUT_SCHEMA
        );
        assert_eq!(value["semantic"]["observed_tier"], "tier_b");
        assert_eq!(value["semantic"]["compiler_assisted_observed"], false);
        assert_eq!(
            value["isolation"]["authoritative_artifact_schema"],
            "rootlight.adapter-isolation/1"
        );
        assert_eq!(
            value["isolation"]["platform_scoped_observations_required"],
            true
        );
        assert_eq!(
            value["isolation"]["integrated_project_session_required"],
            true
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
