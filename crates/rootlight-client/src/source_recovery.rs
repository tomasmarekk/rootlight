//! Source-read recovery admission against the already verified returned file set.
//! One shared IR cause avoids divergent capacity validation across transports;
//! lexical participation never asserts complete structural or semantic support.

use super::*;

pub use rootlight_vfs::{
    AggregateCapacityResource, AggregateCapacityStage, AggregateSourceRecovery,
};

/// How a recovered member participated in lexical indexing, independently of exact reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTextSearchCoverage {
    /// Every source byte was projected; retained vocabulary omissions remain separate.
    WholeSourceProjection,
    /// Exact captured bytes remain readable but body lexical projection was not retained.
    MetadataOnlyCaptured,
    /// Captured bytes remain readable; their encoding has no text projection.
    UnsupportedEncoding,
}

/// Recovery disposition for one distinct returned file in ascending FileId order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct SourceRecoveryFile {
    /// Immutable file identity correlated with returned source references.
    pub file: FileId,
    /// Body participation in text indexing, independently of source readability.
    pub text_search: SourceTextSearchCoverage,
}

/// One generation-wide capacity cause with dispositions for the returned file set.
///
/// The enclosing checked query context binds repository and generation. Public DTO
/// fields may be mutated afterwards; the type alone is not a validation proof.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceReadRecovery {
    /// Actual rejected aggregate capacity and complete source-inventory identity.
    pub cause: AggregateSourceRecovery,
    /// Sorted distinct dispositions exactly covering the returned source chunks.
    pub files: Vec<SourceRecoveryFile>,
}

pub(super) fn parse_source_recovery(
    wire: Option<daemon::SourceReadRecovery>,
    returned_files: impl Iterator<Item = FileId>,
    minor: u32,
) -> Result<Option<SourceReadRecovery>, ClientError> {
    let Some(wire) = wire else {
        return Ok(None);
    };
    if minor < rootlight_protocol::AGGREGATE_SOURCE_RECOVERY_PROTOCOL_MINOR {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let mut expected = Vec::new();
    expected
        .try_reserve_exact(32)
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    for file in returned_files {
        if expected.len() == 32 {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        expected.push(file);
    }
    expected.sort_unstable();
    expected.dedup();
    if expected.is_empty() || wire.files.len() != expected.len() || wire.files.len() > 32 {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let cause = wire.cause.ok_or(ClientError::InvalidResponseCorrelation)?;
    let stage = match daemon::AggregateSourceRecoveryStage::try_from(cause.stage) {
        Ok(daemon::AggregateSourceRecoveryStage::StructuralAppend) => {
            AggregateCapacityStage::StructuralAppend
        }
        Ok(daemon::AggregateSourceRecoveryStage::GenerationText) => {
            AggregateCapacityStage::GenerationText
        }
        _ => return Err(ClientError::InvalidResponseCorrelation),
    };
    let resource = match daemon::AggregateSourceRecoveryResource::try_from(cause.resource) {
        Ok(daemon::AggregateSourceRecoveryResource::Files) => AggregateCapacityResource::Files,
        Ok(daemon::AggregateSourceRecoveryResource::Entities) => {
            AggregateCapacityResource::Entities
        }
        Ok(daemon::AggregateSourceRecoveryResource::Occurrences) => {
            AggregateCapacityResource::Occurrences
        }
        Ok(daemon::AggregateSourceRecoveryResource::Relations) => {
            AggregateCapacityResource::Relations
        }
        Ok(daemon::AggregateSourceRecoveryResource::Provenance) => {
            AggregateCapacityResource::Provenance
        }
        Ok(daemon::AggregateSourceRecoveryResource::SourceMappings) => {
            AggregateCapacityResource::SourceMappings
        }
        Ok(daemon::AggregateSourceRecoveryResource::Coverage) => {
            AggregateCapacityResource::Coverage
        }
        Ok(daemon::AggregateSourceRecoveryResource::SkippedRegions) => {
            AggregateCapacityResource::SkippedRegions
        }
        Ok(daemon::AggregateSourceRecoveryResource::Diagnostics) => {
            AggregateCapacityResource::Diagnostics
        }
        Ok(daemon::AggregateSourceRecoveryResource::Extensions) => {
            AggregateCapacityResource::Extensions
        }
        Ok(daemon::AggregateSourceRecoveryResource::Records) => AggregateCapacityResource::Records,
        Ok(daemon::AggregateSourceRecoveryResource::NestedItems) => {
            AggregateCapacityResource::NestedItems
        }
        Ok(daemon::AggregateSourceRecoveryResource::StringBytes) => {
            AggregateCapacityResource::StringBytes
        }
        Ok(daemon::AggregateSourceRecoveryResource::ExtensionBytes) => {
            AggregateCapacityResource::ExtensionBytes
        }
        Ok(daemon::AggregateSourceRecoveryResource::DiagnosticBytes) => {
            AggregateCapacityResource::DiagnosticBytes
        }
        Ok(daemon::AggregateSourceRecoveryResource::GenerationTextBytes) => {
            AggregateCapacityResource::GenerationTextBytes
        }
        Ok(daemon::AggregateSourceRecoveryResource::GenerationEncodedTextBytes) => {
            AggregateCapacityResource::GenerationEncodedTextBytes
        }
        _ => return Err(ClientError::InvalidResponseCorrelation),
    };
    let cause = AggregateSourceRecovery {
        source_inventory: parse_content_hash(cause.source_inventory)?,
        members: cause.members,
        derived_members: cause.derived_members,
        stage,
        resource,
        observed: cause.observed,
        limit: cause.limit,
        text_snapshot_bytes: cause.text_snapshot_bytes,
    };
    if !cause.is_valid()
        || cause.members
            < u64::try_from(expected.len()).map_err(|_| ClientError::InvalidResponseCorrelation)?
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let mut files = Vec::new();
    files
        .try_reserve_exact(wire.files.len())
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    for (entry, expected) in wire.files.into_iter().zip(expected) {
        let file = parse_file(entry.file)?;
        if file != expected {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        let text_search = match daemon::SourceRecoveryTextSearch::try_from(entry.text_search) {
            Ok(daemon::SourceRecoveryTextSearch::WholeSourceProjection) => {
                SourceTextSearchCoverage::WholeSourceProjection
            }
            Ok(daemon::SourceRecoveryTextSearch::MetadataOnlyCaptured) => {
                SourceTextSearchCoverage::MetadataOnlyCaptured
            }
            Ok(daemon::SourceRecoveryTextSearch::UnsupportedEncoding) => {
                SourceTextSearchCoverage::UnsupportedEncoding
            }
            _ => return Err(ClientError::InvalidResponseCorrelation),
        };
        files.push(SourceRecoveryFile { file, text_search });
    }
    Ok(Some(SourceReadRecovery { cause, files }))
}

pub(super) fn validate_recovery_coverage(
    recovery: &Option<SourceReadRecovery>,
    context: &QueryContext,
    tiers: impl Iterator<Item = AnalysisTier>,
) -> Result<(), ClientError> {
    if recovery.is_some()
        && (context.tier != AnalysisTier::TierD
            || context.coverage_status != CoverageStatus::Unknown
            || tiers.into_iter().any(|tier| tier != AnalysisTier::TierD)
            || context.language_coverage.as_ref().is_some_and(|rows| {
                rows.iter().any(|row| {
                    row.tier != AnalysisTier::TierD || row.status != CoverageStatus::Unknown
                })
            }))
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(())
}

#[cfg(test)]
#[path = "source_recovery_tests.rs"]
mod tests;
