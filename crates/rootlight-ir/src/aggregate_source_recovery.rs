//! Authenticated universal explanation for complete-source aggregate recovery.
//! One envelope covers the manifest's complete member set; structural facts
//! remain unknown, and captured text search has an independent explicit gap.

use rootlight_ids::{ContentHash, FactId, GenerationId, RepositoryId, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef};

/// Registered namespace for complete-source recovery after combined capacity.
pub const AGGREGATE_SOURCE_RECOVERY_NAMESPACE: &str = "rootlight.aggregate_source_recovery";
/// Durable marker requiring the exact aggregate extension, unlike legacy fallback.
pub const AGGREGATE_SOURCE_RECOVERY_DIAGNOSTIC: &str = "aggregate-source-recovery";
/// Registered namespace for each exact derived-to-container source ownership.
pub const AGGREGATE_SOURCE_ORIGIN_NAMESPACE: &str = "rootlight.aggregate_source_origin";
/// Source and policy identity revision, independent of any repository name.
pub const AGGREGATE_SOURCE_RECOVERY_REVISION: &[u8] = b"rootlight.aggregate-source-recovery/1";

/// Exact pre-persistence boundary that rejected combined structural output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AggregateCapacityStage {
    /// Individually admitted documents exceeded a combined IR quota.
    StructuralAppend,
    /// Complete canonical generation text exceeded its reader/writer contract.
    GenerationText,
}

/// Closed count or byte family in a genuine aggregate capacity observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AggregateCapacityResource {
    /// Immutable file records.
    Files,
    /// Structural entities.
    Entities,
    /// Source occurrences.
    Occurrences,
    /// Structural relations.
    Relations,
    /// Producer provenance records.
    Provenance,
    /// Source mappings.
    SourceMappings,
    /// Analysis coverage records.
    Coverage,
    /// Explicit skipped regions.
    SkippedRegions,
    /// Diagnostics.
    Diagnostics,
    /// Registered extension envelopes.
    Extensions,
    /// All top-level records.
    Records,
    /// All nested evidence items.
    NestedItems,
    /// All retained non-payload strings.
    StringBytes,
    /// All extension payload bytes.
    ExtensionBytes,
    /// All diagnostic text bytes.
    DiagnosticBytes,
    /// Decoded generation text under its durable reader contract.
    GenerationTextBytes,
    /// Encoded generation text under its durable reader contract.
    GenerationEncodedTextBytes,
}

/// Source-free universal cause bound to every member of the sealed manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AggregateSourceRecovery {
    /// Exact canonical complete original-plus-derived manifest identity.
    pub source_inventory: ContentHash,
    /// Exact number of immutable members, including the normalized anchor.
    pub members: u64,
    /// Exact derived-member count requiring unique authenticated origin envelopes.
    pub derived_members: u64,
    /// Exact pre-persistence rejection boundary.
    pub stage: AggregateCapacityStage,
    /// Exact aggregate count or byte family.
    pub resource: AggregateCapacityResource,
    /// Actual combined observation, excluding arithmetic-overflow sentinels.
    pub observed: u64,
    /// Actual rejected capacity.
    pub limit: u64,
    /// Captured members above this size retain bytes but only lexical metadata.
    pub text_snapshot_bytes: u64,
}

impl AggregateSourceRecovery {
    /// Checks closed cause semantics without asserting the producer's truth.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.members > 0
            && self.derived_members < self.members
            && self.observed > self.limit
            && self.observed != u64::MAX
            && self.text_snapshot_bytes > 0
            && matches!(
                (self.stage, self.resource),
                (
                    AggregateCapacityStage::GenerationText,
                    AggregateCapacityResource::GenerationTextBytes
                        | AggregateCapacityResource::GenerationEncodedTextBytes
                ) | (
                    AggregateCapacityStage::StructuralAppend,
                    AggregateCapacityResource::Files
                        | AggregateCapacityResource::Entities
                        | AggregateCapacityResource::Occurrences
                        | AggregateCapacityResource::Relations
                        | AggregateCapacityResource::Provenance
                        | AggregateCapacityResource::SourceMappings
                        | AggregateCapacityResource::Coverage
                        | AggregateCapacityResource::SkippedRegions
                        | AggregateCapacityResource::Diagnostics
                        | AggregateCapacityResource::Extensions
                        | AggregateCapacityResource::Records
                        | AggregateCapacityResource::NestedItems
                        | AggregateCapacityResource::StringBytes
                        | AggregateCapacityResource::ExtensionBytes
                        | AggregateCapacityResource::DiagnosticBytes
                )
            )
    }
}

/// Unknown, altered, oversized, or semantically inconsistent recovery evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid aggregate source recovery evidence")]
pub struct AggregateSourceRecoveryError;

/// Binds a single universal cause to generation ownership and its anchor source.
///
/// # Errors
/// Rejects foreign ownership, invalid counts/stages, non-whole anchor starts,
/// oversized payloads, and encoding failures. Storage additionally authenticates
/// inventory hash, exact member count and the complete file-only representation.
pub fn new_aggregate_source_recovery_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    recovery: AggregateSourceRecovery,
) -> Result<ExtensionEnvelope, AggregateSourceRecoveryError> {
    if !recovery.is_valid()
        || source.repository() != repository
        || source.generation() != generation
        || source.span().start_byte() != 0
    {
        return Err(AggregateSourceRecoveryError);
    }
    let payload = serde_json::to_string(&recovery).map_err(|_| AggregateSourceRecoveryError)?;
    if payload.len() > 1_024 {
        return Err(AggregateSourceRecoveryError);
    }
    let subject = FactRef::File(source.span().file());
    let identity = serde_json::to_vec(&(
        AGGREGATE_SOURCE_RECOVERY_NAMESPACE,
        "1",
        repository,
        generation,
        provenance,
        &source,
        &payload,
        subject,
    ))
    .map_err(|_| AggregateSourceRecoveryError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.aggregate_source_recovery/v1", &identity).id(),
        repository,
        generation,
        namespace: AGGREGATE_SOURCE_RECOVERY_NAMESPACE.to_owned(),
        version: "1".to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: vec![subject],
        },
    })
}

/// Decodes only canonical version-one evidence with an exact typed identity.
///
/// # Errors
/// Rejects unknown versions, forged IDs/ownership/evidence, altered causes,
/// oversized payloads and noncanonical serialization.
pub fn decode_aggregate_source_recovery_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<AggregateSourceRecovery, AggregateSourceRecoveryError> {
    if envelope.namespace != AGGREGATE_SOURCE_RECOVERY_NAMESPACE || envelope.payload.len() > 1_024 {
        return Err(AggregateSourceRecoveryError);
    }
    let value =
        serde_json::from_str(&envelope.payload).map_err(|_| AggregateSourceRecoveryError)?;
    let expected = new_aggregate_source_recovery_envelope(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope
            .evidence
            .source
            .clone()
            .ok_or(AggregateSourceRecoveryError)?,
        value,
    )?;
    if expected != *envelope {
        return Err(AggregateSourceRecoveryError);
    }
    Ok(value)
}

/// Constructs one exact whole-file extraction origin without inventing IR maps.
///
/// # Errors
/// Rejects foreign ownership, self-origins, non-whole-file starts and encoding
/// failures. Storage verifies both complete spans against authenticated members.
pub fn new_aggregate_source_origin_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    anchor: SourceRef,
    derived: SourceRef,
    container: SourceRef,
) -> Result<ExtensionEnvelope, AggregateSourceRecoveryError> {
    if anchor.repository() != repository
        || anchor.generation() != generation
        || anchor.span().start_byte() != 0
        || derived.repository() != repository
        || container.repository() != repository
        || derived.generation() != generation
        || container.generation() != generation
        || derived.span().file() == container.span().file()
        || derived.span().start_byte() != 0
        || container.span().start_byte() != 0
    {
        return Err(AggregateSourceRecoveryError);
    }
    let payload =
        serde_json::to_string(&(&derived, &container)).map_err(|_| AggregateSourceRecoveryError)?;
    if payload.len() > 1_024 {
        return Err(AggregateSourceRecoveryError);
    }
    let identity = serde_json::to_vec(&(
        AGGREGATE_SOURCE_ORIGIN_NAMESPACE,
        "1",
        repository,
        generation,
        provenance,
        &anchor,
        &payload,
    ))
    .map_err(|_| AggregateSourceRecoveryError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.aggregate_source_origin/v1", &identity).id(),
        repository,
        generation,
        namespace: AGGREGATE_SOURCE_ORIGIN_NAMESPACE.to_owned(),
        version: "1".to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(anchor.clone()),
            derivation: vec![FactRef::File(anchor.span().file())],
        },
    })
}

/// Authenticates the exact version, payload, ownership and origin identity.
///
/// # Errors
/// Rejects altered, noncanonical, oversized, foreign or unknown-version records.
pub fn decode_aggregate_source_origin_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<(SourceRef, SourceRef), AggregateSourceRecoveryError> {
    if envelope.namespace != AGGREGATE_SOURCE_ORIGIN_NAMESPACE || envelope.payload.len() > 1_024 {
        return Err(AggregateSourceRecoveryError);
    }
    let (derived, container) =
        serde_json::from_str(&envelope.payload).map_err(|_| AggregateSourceRecoveryError)?;
    let expected = new_aggregate_source_origin_envelope(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope
            .evidence
            .source
            .clone()
            .ok_or(AggregateSourceRecoveryError)?,
        derived,
        container,
    )?;
    if expected != *envelope {
        return Err(AggregateSourceRecoveryError);
    }
    serde_json::from_str(&envelope.payload).map_err(|_| AggregateSourceRecoveryError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceSpan;
    use rootlight_ids::{FileId, content_hash};

    fn source(file: u8) -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(FileId::from_bytes([file; 20]), 0, 10).unwrap(),
            content_hash(&[file; 10]),
            None,
        )
    }

    fn cause() -> AggregateSourceRecovery {
        AggregateSourceRecovery {
            source_inventory: content_hash(b"complete manifest"),
            members: 3,
            derived_members: 1,
            stage: AggregateCapacityStage::StructuralAppend,
            resource: AggregateCapacityResource::Entities,
            observed: 4,
            limit: 3,
            text_snapshot_bytes: 4096,
        }
    }

    #[test]
    fn aggregate_cause_rejects_unknown_schema_and_every_unauthenticated_payload_mutation() {
        let anchor = source(3);
        let envelope = new_aggregate_source_recovery_envelope(
            anchor.repository(),
            anchor.generation(),
            FactId::from_bytes([4; 20]),
            anchor,
            cause(),
        )
        .unwrap();
        assert_eq!(
            decode_aggregate_source_recovery_envelope(&envelope).unwrap(),
            cause()
        );
        for field in [
            "source_inventory",
            "members",
            "derived_members",
            "stage",
            "resource",
            "observed",
            "limit",
            "text_snapshot_bytes",
        ] {
            let mut altered = envelope.clone();
            let mut payload: serde_json::Value = serde_json::from_str(&altered.payload).unwrap();
            payload[field] = serde_json::Value::Null;
            altered.payload = serde_json::to_string(&payload).unwrap();
            assert!(
                decode_aggregate_source_recovery_envelope(&altered).is_err(),
                "{field}"
            );
        }
        let mut unknown = envelope;
        unknown.version = "2".to_owned();
        assert!(decode_aggregate_source_recovery_envelope(&unknown).is_err());
        for invalid in [
            AggregateSourceRecovery {
                members: 0,
                ..cause()
            },
            AggregateSourceRecovery {
                derived_members: 3,
                ..cause()
            },
            AggregateSourceRecovery {
                observed: 3,
                ..cause()
            },
            AggregateSourceRecovery {
                observed: u64::MAX,
                ..cause()
            },
            AggregateSourceRecovery {
                stage: AggregateCapacityStage::GenerationText,
                ..cause()
            },
        ] {
            assert!(!invalid.is_valid());
        }
    }

    #[test]
    fn origin_envelopes_bind_exact_raw_sources_and_never_use_catalog_files_as_ir_evidence() {
        let anchor = source(3);
        let derived = source(5);
        let container = source(6);
        let envelope = new_aggregate_source_origin_envelope(
            anchor.repository(),
            anchor.generation(),
            FactId::from_bytes([4; 20]),
            anchor.clone(),
            derived.clone(),
            container.clone(),
        )
        .unwrap();
        assert_eq!(
            decode_aggregate_source_origin_envelope(&envelope).unwrap(),
            (derived.clone(), container.clone())
        );
        assert_eq!(envelope.evidence.source, Some(anchor.clone()));
        assert_eq!(
            envelope.evidence.derivation,
            [FactRef::File(anchor.span().file())]
        );
        assert!(
            new_aggregate_source_origin_envelope(
                anchor.repository(),
                anchor.generation(),
                FactId::from_bytes([4; 20]),
                anchor,
                derived.clone(),
                derived
            )
            .is_err()
        );
        let mut corrupt = envelope;
        corrupt.payload.push(' ');
        assert!(decode_aggregate_source_origin_envelope(&corrupt).is_err());
    }
}
