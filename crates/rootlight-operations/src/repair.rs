//! Source-free dry-run planning for offline catalog recovery.
//!
//! Repair planning inventories bounded verified generation metadata and never
//! mutates or names paths outside the caller-owned state directory.

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

use crate::OperationJournal;

/// Schema version for serialized repair plans and reconstruction inventories.
pub const REPAIR_SCHEMA_VERSION: &str = "1.0";
/// Maximum number of generation manifests accepted by one repair request.
pub const MAX_REPAIR_CANDIDATES: usize = 4_096;
/// Maximum aggregate byte claim accepted from a reconstruction inventory.
pub const MAX_REPAIR_REQUIRED_BYTES: u64 = 4 * 1024 * 1024 * 1024 * 1024;

const MAX_REPAIR_LABEL_BYTES: usize = 128;
const RECONSTRUCTED_CATALOG_BASE_BYTES: u64 = 16 * 1024;
const RECONSTRUCTED_CATALOG_BYTES_PER_GENERATION: u64 = 512;

/// Closed set of repair operations exposed by the command-line contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairAction {
    /// Validate catalog identity, schema, policy, and SQLite integrity.
    VerifyCatalog,
    /// Validate immutable generation headers and manifest identities.
    VerifyGenerationHeaders,
    /// Perform every bounded read-only storage validation.
    FullScrub,
    /// Plan activation of the newest verified retained generation.
    SelectLastGoodGeneration,
    /// Plan replacement of a generation-aligned lexical index.
    RebuildLexicalIndex,
    /// Plan replacement of recomputable derived facts.
    RebuildDerivedOverlays,
    /// Plan a clean repository rebuild from registered source.
    RebuildRepository,
    /// Plan reconstruction of catalog records from verified manifests.
    ReconstructCatalogFromManifests,
    /// Plan removal of explicitly quarantined, non-active artifacts.
    PurgeQuarantine,
}

/// Source-free generation metadata accepted for catalog reconstruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationRepairCandidate {
    /// Stable generation identifier.
    pub generation_id: String,
    /// Lowercase SHA-256 of the complete generation manifest.
    pub manifest_sha256: String,
    /// Whether publication wrote the complete-generation marker.
    pub complete: bool,
    /// Whether all declared artifacts passed independent verification.
    pub verified: bool,
    /// Additional disk bytes required to retain this candidate during recovery.
    pub required_bytes: u64,
}

/// Source-free classification of the catalog inspected by a repair plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairCatalogStatus {
    /// The catalog passed its defensive read-only validation.
    Healthy,
    /// No catalog exists at the owned catalog location.
    Missing,
    /// A catalog artifact exists but failed safe validation.
    Invalid,
}

/// Closed disposition for a repair plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairPlanStatus {
    /// The requested read-only validation passed and no writes are needed.
    NoChange,
    /// The requested operation has a bounded, non-destructive write plan.
    Ready,
    /// The operation cannot proceed without a safer implementation or evidence.
    Blocked,
}

/// Stable reason why a repair plan cannot safely execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairBlockReason {
    /// Catalog validation failed and the requested action cannot repair it.
    CatalogInvalid,
    /// Catalog reconstruction has no complete independently verified generation.
    NoVerifiedGeneration,
    /// The action is declared but its mutation path is not enabled.
    ActionUnavailable,
}

/// Closed write kind proposed by a dry-run repair plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairWriteKind {
    /// Create a new private artifact without replacing an existing path.
    CreateNew,
    /// Preserve the existing catalog as a recovery copy.
    PreserveBackup,
}

/// One caller-owned relative write proposed by a repair plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepairWrite {
    /// Fixed path relative to the private state directory.
    pub relative_path: &'static str,
    /// Write behavior that an applying implementation must preserve.
    pub kind: RepairWriteKind,
    /// Conservative upper bound for the proposed write.
    pub maximum_bytes: u64,
}

/// Rollback guarantee retained by a repair plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairRollback {
    /// The operation is read-only.
    NotRequired,
    /// The original catalog remains untouched until replacement verification.
    OriginalPreserved,
    /// No safe rollback path is implemented for the requested action.
    Unavailable,
}

/// Deterministic source-free result of repair preflight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepairPlan {
    /// Repair contract schema version.
    pub schema_version: &'static str,
    /// Requested repair action.
    pub action: RepairAction,
    /// Repair commands default to non-mutating behavior.
    pub dry_run: bool,
    /// Observed catalog classification.
    pub catalog_status: RepairCatalogStatus,
    /// Whether the action can proceed under the current evidence.
    pub status: RepairPlanStatus,
    /// Stable blocking reason when the action cannot proceed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<RepairBlockReason>,
    /// Stable generation identifiers affected by the plan.
    pub affected_generation_ids: Vec<String>,
    /// Complete allow-list of proposed relative writes.
    pub proposed_writes: Vec<RepairWrite>,
    /// Conservative additional disk requirement.
    pub required_disk_bytes: u64,
    /// Whether repository source is needed to apply the plan.
    pub source_required: bool,
    /// Rollback guarantee for the proposed action.
    pub rollback: RepairRollback,
}

/// Failure to construct a bounded, deterministic repair plan.
#[derive(Debug, thiserror::Error)]
pub enum RepairError {
    /// The catalog path could not be inspected safely.
    #[error("catalog metadata inspection failed")]
    CatalogMetadata(#[source] io::Error),
    /// The reconstruction inventory exceeds a hard bound.
    #[error("repair inventory exceeds {0}")]
    LimitExceeded(&'static str),
    /// The reconstruction inventory is not canonical or internally consistent.
    #[error("repair inventory is invalid")]
    InvalidInventory,
}

/// Builds a non-destructive repair plan for one owned catalog path.
///
/// Candidates must be strictly ordered by generation ID. Reconstruction uses
/// only complete, independently verified manifests and never proposes deletion
/// or in-place mutation of the existing catalog.
///
/// # Errors
///
/// Returns [`RepairError`] when catalog metadata cannot be inspected, inventory
/// bounds are exceeded, or candidate identities are not canonical.
pub fn plan_catalog_repair(
    catalog_path: &Path,
    action: RepairAction,
    candidates: &[GenerationRepairCandidate],
) -> Result<RepairPlan, RepairError> {
    validate_candidates(candidates)?;
    let catalog_status = classify_catalog(catalog_path)?;

    match action {
        RepairAction::VerifyCatalog => Ok(verify_catalog_plan(action, catalog_status)),
        RepairAction::ReconstructCatalogFromManifests => {
            reconstruct_catalog_plan(action, catalog_status, candidates)
        }
        RepairAction::VerifyGenerationHeaders
        | RepairAction::FullScrub
        | RepairAction::SelectLastGoodGeneration
        | RepairAction::RebuildLexicalIndex
        | RepairAction::RebuildDerivedOverlays
        | RepairAction::RebuildRepository
        | RepairAction::PurgeQuarantine => Ok(blocked_plan(
            action,
            catalog_status,
            RepairBlockReason::ActionUnavailable,
            action_requires_source(action),
        )),
    }
}

fn classify_catalog(catalog_path: &Path) -> Result<RepairCatalogStatus, RepairError> {
    let metadata = match fs::symlink_metadata(catalog_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(RepairCatalogStatus::Missing);
        }
        Err(error) => return Err(RepairError::CatalogMetadata(error)),
    };
    if !metadata.file_type().is_file() {
        return Ok(RepairCatalogStatus::Invalid);
    }
    Ok(
        if OperationJournal::quick_check_path(catalog_path).is_ok() {
            RepairCatalogStatus::Healthy
        } else {
            RepairCatalogStatus::Invalid
        },
    )
}

fn verify_catalog_plan(action: RepairAction, catalog_status: RepairCatalogStatus) -> RepairPlan {
    if catalog_status == RepairCatalogStatus::Healthy {
        RepairPlan {
            schema_version: REPAIR_SCHEMA_VERSION,
            action,
            dry_run: true,
            catalog_status,
            status: RepairPlanStatus::NoChange,
            blocked_reason: None,
            affected_generation_ids: Vec::new(),
            proposed_writes: Vec::new(),
            required_disk_bytes: 0,
            source_required: false,
            rollback: RepairRollback::NotRequired,
        }
    } else {
        blocked_plan(
            action,
            catalog_status,
            RepairBlockReason::CatalogInvalid,
            false,
        )
    }
}

fn reconstruct_catalog_plan(
    action: RepairAction,
    catalog_status: RepairCatalogStatus,
    candidates: &[GenerationRepairCandidate],
) -> Result<RepairPlan, RepairError> {
    let verified = candidates
        .iter()
        .filter(|candidate| candidate.complete && candidate.verified)
        .collect::<Vec<_>>();
    if verified.is_empty() {
        return Ok(blocked_plan(
            action,
            catalog_status,
            RepairBlockReason::NoVerifiedGeneration,
            false,
        ));
    }

    let affected_generation_ids = verified
        .iter()
        .map(|candidate| candidate.generation_id.clone())
        .collect::<Vec<_>>();
    let catalog_bytes = u64::try_from(verified.len())
        .ok()
        .and_then(|count| count.checked_mul(RECONSTRUCTED_CATALOG_BYTES_PER_GENERATION))
        .and_then(|bytes| bytes.checked_add(RECONSTRUCTED_CATALOG_BASE_BYTES))
        .ok_or(RepairError::LimitExceeded("required_disk_bytes"))?;
    let retained_bytes = verified.iter().try_fold(0_u64, |total, candidate| {
        total.checked_add(candidate.required_bytes)
    });
    let required_disk_bytes = retained_bytes
        .and_then(|bytes| bytes.checked_add(catalog_bytes))
        .filter(|bytes| *bytes <= MAX_REPAIR_REQUIRED_BYTES)
        .ok_or(RepairError::LimitExceeded("required_disk_bytes"))?;

    let mut proposed_writes = Vec::with_capacity(2);
    if catalog_status != RepairCatalogStatus::Missing {
        proposed_writes.push(RepairWrite {
            relative_path: "catalog.sqlite3.recovery-copy",
            kind: RepairWriteKind::PreserveBackup,
            maximum_bytes: required_disk_bytes,
        });
    }
    proposed_writes.push(RepairWrite {
        relative_path: "catalog.sqlite3.reconstructed",
        kind: RepairWriteKind::CreateNew,
        maximum_bytes: catalog_bytes,
    });

    Ok(RepairPlan {
        schema_version: REPAIR_SCHEMA_VERSION,
        action,
        dry_run: true,
        catalog_status,
        status: RepairPlanStatus::Ready,
        blocked_reason: None,
        affected_generation_ids,
        proposed_writes,
        required_disk_bytes,
        source_required: false,
        rollback: RepairRollback::OriginalPreserved,
    })
}

fn blocked_plan(
    action: RepairAction,
    catalog_status: RepairCatalogStatus,
    blocked_reason: RepairBlockReason,
    source_required: bool,
) -> RepairPlan {
    RepairPlan {
        schema_version: REPAIR_SCHEMA_VERSION,
        action,
        dry_run: true,
        catalog_status,
        status: RepairPlanStatus::Blocked,
        blocked_reason: Some(blocked_reason),
        affected_generation_ids: Vec::new(),
        proposed_writes: Vec::new(),
        required_disk_bytes: 0,
        source_required,
        rollback: RepairRollback::Unavailable,
    }
}

const fn action_requires_source(action: RepairAction) -> bool {
    matches!(
        action,
        RepairAction::RebuildRepository | RepairAction::RebuildDerivedOverlays
    )
}

fn validate_candidates(candidates: &[GenerationRepairCandidate]) -> Result<(), RepairError> {
    if candidates.len() > MAX_REPAIR_CANDIDATES {
        return Err(RepairError::LimitExceeded("generation_count"));
    }
    let mut previous: Option<&str> = None;
    let mut aggregate_bytes = 0_u64;
    for candidate in candidates {
        if !valid_label(&candidate.generation_id)
            || !valid_sha256(&candidate.manifest_sha256)
            || previous.is_some_and(|value| value >= candidate.generation_id.as_str())
        {
            return Err(RepairError::InvalidInventory);
        }
        aggregate_bytes = aggregate_bytes
            .checked_add(candidate.required_bytes)
            .filter(|bytes| *bytes <= MAX_REPAIR_REQUIRED_BYTES)
            .ok_or(RepairError::LimitExceeded("required_disk_bytes"))?;
        previous = Some(&candidate.generation_id);
    }
    Ok(())
}

fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REPAIR_LABEL_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use tempfile::tempdir;

    use super::*;

    fn candidate(id: &str, byte: char) -> GenerationRepairCandidate {
        GenerationRepairCandidate {
            generation_id: id.to_owned(),
            manifest_sha256: std::iter::repeat_n(byte, 64).collect(),
            complete: true,
            verified: true,
            required_bytes: 4_096,
        }
    }

    #[test]
    fn healthy_catalog_verification_is_read_only() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        OperationJournal::open(&path).expect("catalog opens");

        let plan = plan_catalog_repair(&path, RepairAction::VerifyCatalog, &[])
            .expect("repair plan builds");

        assert_eq!(plan.status, RepairPlanStatus::NoChange);
        assert_eq!(plan.catalog_status, RepairCatalogStatus::Healthy);
        assert!(plan.proposed_writes.is_empty());
        assert_eq!(plan.rollback, RepairRollback::NotRequired);
    }

    #[test]
    fn invalid_catalog_verification_fails_closed_without_disclosing_path() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        File::create(&path).expect("invalid catalog fixture creates");

        let plan = plan_catalog_repair(&path, RepairAction::VerifyCatalog, &[])
            .expect("invalid state remains a typed plan");
        let encoded = serde_json::to_string(&plan).expect("plan serializes");

        assert_eq!(plan.status, RepairPlanStatus::Blocked);
        assert_eq!(plan.blocked_reason, Some(RepairBlockReason::CatalogInvalid));
        assert!(!encoded.contains(temporary.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn reconstruction_uses_only_complete_verified_candidates() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        let mut incomplete = candidate("generation-02", 'b');
        incomplete.complete = false;
        let candidates = [candidate("generation-01", 'a'), incomplete];

        let plan = plan_catalog_repair(
            &path,
            RepairAction::ReconstructCatalogFromManifests,
            &candidates,
        )
        .expect("reconstruction plan builds");

        assert_eq!(plan.status, RepairPlanStatus::Ready);
        assert_eq!(plan.catalog_status, RepairCatalogStatus::Missing);
        assert_eq!(plan.affected_generation_ids, ["generation-01"]);
        assert_eq!(plan.proposed_writes.len(), 1);
        assert_eq!(
            plan.proposed_writes[0].relative_path,
            "catalog.sqlite3.reconstructed"
        );
        assert_eq!(plan.rollback, RepairRollback::OriginalPreserved);
    }

    #[test]
    fn reconstruction_preserves_an_existing_catalog_before_new_output() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        File::create(&path).expect("invalid catalog fixture creates");

        let plan = plan_catalog_repair(
            &path,
            RepairAction::ReconstructCatalogFromManifests,
            &[candidate("generation-01", 'a')],
        )
        .expect("reconstruction plan builds");

        assert_eq!(
            plan.proposed_writes
                .iter()
                .map(|write| write.kind)
                .collect::<Vec<_>>(),
            [RepairWriteKind::PreserveBackup, RepairWriteKind::CreateNew]
        );
        assert!(
            plan.proposed_writes
                .iter()
                .all(|write| !write.relative_path.contains("delete"))
        );
    }

    #[test]
    fn reconstruction_requires_canonical_bounded_inventory() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        let unsorted = [
            candidate("generation-02", 'b'),
            candidate("generation-01", 'a'),
        ];
        assert!(matches!(
            plan_catalog_repair(
                &path,
                RepairAction::ReconstructCatalogFromManifests,
                &unsorted
            ),
            Err(RepairError::InvalidInventory)
        ));

        let mut oversized = candidate("generation-01", 'a');
        oversized.required_bytes = MAX_REPAIR_REQUIRED_BYTES + 1;
        assert!(matches!(
            plan_catalog_repair(
                &path,
                RepairAction::ReconstructCatalogFromManifests,
                &[oversized]
            ),
            Err(RepairError::LimitExceeded("required_disk_bytes"))
        ));
    }

    #[test]
    fn unimplemented_mutations_are_explicitly_blocked() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");

        let plan = plan_catalog_repair(&path, RepairAction::RebuildRepository, &[])
            .expect("repair plan builds");

        assert_eq!(plan.status, RepairPlanStatus::Blocked);
        assert_eq!(
            plan.blocked_reason,
            Some(RepairBlockReason::ActionUnavailable)
        );
        assert!(plan.source_required);
        assert!(plan.proposed_writes.is_empty());
    }
}
