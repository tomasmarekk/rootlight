//! Projects authenticated recovery once per source response, after source admission.
//! All distinct returned members must agree on their universal generation cause;
//! legacy protocol readers retain exact bytes without an invented cause.

use super::*;
use rootlight_ir::{AggregateCapacityResource, AggregateCapacityStage, AggregateSourceRecovery};
use rootlight_service::{FirstSliceSourceRecovery, FirstSliceTextSearchCoverage};

pub(super) fn source_recovery_to_wire(
    service: &FirstSliceService,
    generation: GenerationId,
    chunks: &[daemon::FirstSliceSourceChunk],
    context: &FirstSliceIpcContext,
) -> Result<Option<daemon::SourceReadRecovery>, PublicError> {
    if context.selected_protocol_minor
        < rootlight_protocol::AGGREGATE_SOURCE_RECOVERY_PROTOCOL_MINOR
    {
        return Ok(None);
    }
    let mut members = BTreeSet::new();
    for chunk in chunks {
        let source = chunk.source.as_ref().ok_or_else(internal_error)?;
        members.insert(parse_file(source.file.as_ref())?);
    }
    let mut records = Vec::new();
    records
        .try_reserve_exact(members.len())
        .map_err(|_| resource_exhausted())?;
    for file in members {
        let recovery = service
            .source_file_recovery_until(generation, file, &context.cancellation)
            .map_err(service_error)?;
        records.push((file, recovery));
    }
    recovery_records_to_wire(records)
}

fn recovery_records_to_wire(
    records: Vec<(FileId, Option<FirstSliceSourceRecovery>)>,
) -> Result<Option<daemon::SourceReadRecovery>, PublicError> {
    let Some((_, first)) = records.first() else {
        return Ok(None);
    };
    let cause = first.map(|recovery| recovery.cause);
    if records.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        || records
            .iter()
            .any(|(_, value)| value.map(|recovery| recovery.cause) != cause)
    {
        return Err(internal_error());
    }
    let Some(cause) = cause else {
        return Ok(None);
    };
    if !cause.is_valid()
        || cause.members < u64::try_from(records.len()).map_err(|_| internal_error())?
    {
        return Err(internal_error());
    }
    let mut files = Vec::new();
    files
        .try_reserve_exact(records.len())
        .map_err(|_| resource_exhausted())?;
    for (file, value) in records {
        let value = value.ok_or_else(internal_error)?;
        let text_search = match value.text_search {
            FirstSliceTextSearchCoverage::WholeSourceProjection => {
                daemon::SourceRecoveryTextSearch::WholeSourceProjection
            }
            FirstSliceTextSearchCoverage::MetadataOnlyCaptured => {
                daemon::SourceRecoveryTextSearch::MetadataOnlyCaptured
            }
            FirstSliceTextSearchCoverage::UnsupportedEncoding => {
                daemon::SourceRecoveryTextSearch::UnsupportedEncoding
            }
        };
        files.push(daemon::SourceRecoveryFile {
            file: Some(file_to_wire(file)),
            text_search: text_search as i32,
        });
    }
    Ok(Some(daemon::SourceReadRecovery {
        cause: Some(cause_to_wire(cause)),
        files,
    }))
}

fn cause_to_wire(cause: AggregateSourceRecovery) -> daemon::AggregateSourceRecoveryCause {
    let stage = match cause.stage {
        AggregateCapacityStage::StructuralAppend => {
            daemon::AggregateSourceRecoveryStage::StructuralAppend
        }
        AggregateCapacityStage::GenerationText => {
            daemon::AggregateSourceRecoveryStage::GenerationText
        }
    };
    let resource = match cause.resource {
        AggregateCapacityResource::Files => daemon::AggregateSourceRecoveryResource::Files,
        AggregateCapacityResource::Entities => daemon::AggregateSourceRecoveryResource::Entities,
        AggregateCapacityResource::Occurrences => {
            daemon::AggregateSourceRecoveryResource::Occurrences
        }
        AggregateCapacityResource::Relations => daemon::AggregateSourceRecoveryResource::Relations,
        AggregateCapacityResource::Provenance => {
            daemon::AggregateSourceRecoveryResource::Provenance
        }
        AggregateCapacityResource::SourceMappings => {
            daemon::AggregateSourceRecoveryResource::SourceMappings
        }
        AggregateCapacityResource::Coverage => daemon::AggregateSourceRecoveryResource::Coverage,
        AggregateCapacityResource::SkippedRegions => {
            daemon::AggregateSourceRecoveryResource::SkippedRegions
        }
        AggregateCapacityResource::Diagnostics => {
            daemon::AggregateSourceRecoveryResource::Diagnostics
        }
        AggregateCapacityResource::Extensions => {
            daemon::AggregateSourceRecoveryResource::Extensions
        }
        AggregateCapacityResource::Records => daemon::AggregateSourceRecoveryResource::Records,
        AggregateCapacityResource::NestedItems => {
            daemon::AggregateSourceRecoveryResource::NestedItems
        }
        AggregateCapacityResource::StringBytes => {
            daemon::AggregateSourceRecoveryResource::StringBytes
        }
        AggregateCapacityResource::ExtensionBytes => {
            daemon::AggregateSourceRecoveryResource::ExtensionBytes
        }
        AggregateCapacityResource::DiagnosticBytes => {
            daemon::AggregateSourceRecoveryResource::DiagnosticBytes
        }
        AggregateCapacityResource::GenerationTextBytes => {
            daemon::AggregateSourceRecoveryResource::GenerationTextBytes
        }
        AggregateCapacityResource::GenerationEncodedTextBytes => {
            daemon::AggregateSourceRecoveryResource::GenerationEncodedTextBytes
        }
    };
    daemon::AggregateSourceRecoveryCause {
        source_inventory: Some(content_hash_to_wire(cause.source_inventory)),
        members: cause.members,
        derived_members: cause.derived_members,
        stage: stage as i32,
        resource: resource as i32,
        observed: cause.observed,
        limit: cause.limit,
        text_snapshot_bytes: cause.text_snapshot_bytes,
    }
}

#[cfg(test)]
#[path = "source_recovery_tests.rs"]
mod tests;
