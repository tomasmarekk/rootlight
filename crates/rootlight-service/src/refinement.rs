//! Preserves source evidence when project analysis refines structural output.
//! Project records remain authoritative; missing occurrences retain their original
//! identity and the transitive evidence needed to validate and query them.

use super::{
    Cancellation, FirstSliceError, FirstSliceResource, check_cancellation, checked_resource_length,
    extension_payload_bytes, normalized_record_count,
};
use std::collections::{BTreeMap, BTreeSet};

use rootlight_ids::{ContentHash, FactId, FileId, GenerationId, RepositoryId};
use rootlight_ir::{
    ContainerRef, CoverageRecord, CoverageScope, CoverageStatus, DiagnosticRecord, EntityRecord,
    ExtensionEnvelope, FactDomain, FactEvidence, FactRef, FileRecord, IrLimits,
    LEXICAL_EXTENSION_NAMESPACE, NormalizedIrDocument, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, ProvenanceRecord, RelationEndpoint, RelationRecord,
    SYMBOL_IDENTITY_CLAIM_NAMESPACE, SkippedRegion, SourceMappingRecord, SourceSpan,
    derive_coverage_record_id,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Record<'a> {
    File(&'a FileRecord),
    Entity(&'a EntityRecord),
    Occurrence(&'a OccurrenceRecord),
    Relation(&'a RelationRecord),
    Provenance(&'a ProvenanceRecord),
    Mapping(&'a SourceMappingRecord),
    Coverage(&'a CoverageRecord),
    Skipped(&'a SkippedRegion),
    Diagnostic(&'a DiagnosticRecord),
    Extension(&'a ExtensionEnvelope),
}

impl Record<'_> {
    fn key(self) -> FactRef {
        match self {
            Self::File(record) => FactRef::File(record.id),
            Self::Entity(record) => FactRef::Entity(record.id),
            Self::Occurrence(record) => FactRef::Fact(record.id),
            Self::Relation(record) => FactRef::Fact(record.id),
            Self::Provenance(record) => FactRef::Fact(record.id),
            Self::Mapping(record) => FactRef::Fact(record.id),
            Self::Coverage(record) => FactRef::Fact(record.id),
            Self::Skipped(record) => FactRef::Fact(record.id),
            Self::Diagnostic(record) => FactRef::Fact(record.id),
            Self::Extension(record) => FactRef::Fact(record.id),
        }
    }

    fn slot(self) -> usize {
        match self {
            Self::File(_) => 0,
            Self::Entity(_) => 1,
            Self::Occurrence(_) => 2,
            Self::Relation(_) => 3,
            Self::Provenance(_) => 4,
            Self::Mapping(_) => 5,
            Self::Coverage(_) => 6,
            Self::Skipped(_) => 7,
            Self::Diagnostic(_) => 8,
            Self::Extension(_) => 9,
        }
    }

    fn dependencies(self, mut visit: impl FnMut(FactRef)) {
        let evidence: Option<(_, &FactEvidence)> = match self {
            Self::File(record) => Some((record.provenance, &record.evidence)),
            Self::Entity(record) => {
                match record.container {
                    Some(ContainerRef::File(file)) => visit(FactRef::File(file)),
                    Some(ContainerRef::Entity(entity)) => visit(FactRef::Entity(entity)),
                    Some(ContainerRef::Repository(_)) | None => {}
                }
                Some((record.provenance, &record.evidence))
            }
            Self::Occurrence(record) => {
                visit(FactRef::File(record.file));
                if let Some(enclosing) = record.enclosing {
                    visit(FactRef::Entity(enclosing));
                }
                match &record.target {
                    OccurrenceTarget::Resolved { symbol } => visit(FactRef::Entity(*symbol)),
                    OccurrenceTarget::Candidates { symbols, .. } => {
                        for symbol in symbols {
                            visit(FactRef::Entity(*symbol));
                        }
                    }
                    OccurrenceTarget::Unresolved { .. } => {}
                }
                visit(FactRef::File(record.source.span().file()));
                Some((record.provenance, &record.evidence))
            }
            Self::Relation(record) => {
                for endpoint in [record.subject, record.object] {
                    if let Some(reference) = endpoint_reference(endpoint) {
                        visit(reference);
                    }
                }
                Some((record.provenance, &record.evidence))
            }
            Self::Provenance(record) => {
                for source in record.input_sources.iter().chain(&record.evidence_sources) {
                    visit(FactRef::File(source.span().file()));
                }
                for reference in &record.derivation_parents {
                    visit(*reference);
                }
                None
            }
            Self::Mapping(record) => {
                visit(FactRef::File(record.from.span().file()));
                visit(FactRef::File(record.to.span().file()));
                Some((record.provenance, &record.evidence))
            }
            Self::Coverage(record) => {
                match record.scope {
                    CoverageScope::File(file) => visit(FactRef::File(file)),
                    CoverageScope::Entity(entity) => visit(FactRef::Entity(entity)),
                    CoverageScope::Repository(_) => {}
                }
                Some((record.provenance, &record.evidence))
            }
            Self::Skipped(record) => {
                visit(FactRef::File(record.source.span().file()));
                Some((record.provenance, &record.evidence))
            }
            Self::Diagnostic(record) => {
                if let Some(source) = &record.source {
                    visit(FactRef::File(source.span().file()));
                }
                Some((record.provenance, &record.evidence))
            }
            Self::Extension(record) => Some((record.provenance, &record.evidence)),
        };
        if let Some((provenance, evidence)) = evidence {
            visit(FactRef::Fact(provenance));
            if let Some(source) = &evidence.source {
                visit(FactRef::File(source.span().file()));
            }
            for reference in &evidence.derivation {
                visit(*reference);
            }
        }
    }

    fn append(self, document: &mut NormalizedIrDocument) {
        match self {
            Self::File(record) => document.files.push(record.clone()),
            Self::Entity(record) => document.entities.push(record.clone()),
            Self::Occurrence(record) => document.occurrences.push(record.clone()),
            Self::Relation(record) => document.relations.push(record.clone()),
            Self::Provenance(record) => document.provenance.push(record.clone()),
            Self::Mapping(record) => document.source_mappings.push(record.clone()),
            Self::Coverage(record) => document.coverage_records.push(record.clone()),
            Self::Skipped(record) => document.skipped_regions.push(record.clone()),
            Self::Diagnostic(record) => document.diagnostics.push(record.clone()),
            Self::Extension(record) => document.extensions.push(record.clone()),
        }
    }
}

fn records(document: &NormalizedIrDocument) -> impl Iterator<Item = Record<'_>> {
    document
        .files
        .iter()
        .map(Record::File)
        .chain(document.entities.iter().map(Record::Entity))
        .chain(document.occurrences.iter().map(Record::Occurrence))
        .chain(document.relations.iter().map(Record::Relation))
        .chain(document.provenance.iter().map(Record::Provenance))
        .chain(document.source_mappings.iter().map(Record::Mapping))
        .chain(document.coverage_records.iter().map(Record::Coverage))
        .chain(document.skipped_regions.iter().map(Record::Skipped))
        .chain(document.diagnostics.iter().map(Record::Diagnostic))
        .chain(document.extensions.iter().map(Record::Extension))
}

fn endpoint_reference(endpoint: RelationEndpoint) -> Option<FactRef> {
    match endpoint {
        RelationEndpoint::Repository(_) => None,
        RelationEndpoint::File(file) => Some(FactRef::File(file)),
        RelationEndpoint::Entity(entity) => Some(FactRef::Entity(entity)),
        RelationEndpoint::Occurrence(occurrence) => Some(FactRef::Fact(occurrence)),
    }
}

fn occurrence_site(
    record: &OccurrenceRecord,
) -> (
    OccurrenceRole,
    RepositoryId,
    GenerationId,
    SourceSpan,
    ContentHash,
) {
    // Line hints are presentation-only; differing hints cannot create another
    // occurrence at the same authoritative generation-bound byte span.
    (
        record.role,
        record.source.repository(),
        record.source.generation(),
        record.source.span(),
        record.source.content_hash(),
    )
}

/// Retains missing source occurrences and their validated structural evidence.
///
/// Existing project records take precedence. Supplemental coverage describes only
/// retained evidence from its original producer. Returns a typed identity,
/// cancellation or resource error before the caller can append an incomplete
/// evidence graph to the publication document.
pub(super) fn retain_structural_occurrences(
    mut project: NormalizedIrDocument,
    structural: &[NormalizedIrDocument],
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<NormalizedIrDocument, FirstSliceError> {
    check_cancellation(cancellation)?;
    checked_resource_length(
        0,
        normalized_record_count(&project)?,
        limits.max_total_records,
        FirstSliceResource::Records,
    )?;
    let mut available = BTreeSet::new();
    for record in records(&project) {
        check_cancellation(cancellation)?;
        available.insert(record.key());
    }
    let sites = project
        .occurrences
        .iter()
        .map(occurrence_site)
        .collect::<BTreeSet<_>>();
    let mut pending = BTreeSet::new();
    let mut affected_files = BTreeSet::new();
    let mut input_count = 0;
    for document in structural {
        check_cancellation(cancellation)?;
        input_count = checked_resource_length(
            input_count,
            normalized_record_count(document)?,
            limits.max_total_records,
            FirstSliceResource::Records,
        )?;
        if document.version != project.version
            || document.repository != project.repository
            || document.generation != project.generation
        {
            return Err(FirstSliceError::Identity);
        }
        for occurrence in &document.occurrences {
            check_cancellation(cancellation)?;
            if !sites.contains(&occurrence_site(occurrence)) {
                pending.insert(FactRef::Fact(occurrence.id));
                affected_files.insert(occurrence.file);
            }
        }
    }
    if pending.is_empty() {
        return Ok(project);
    }

    let mut index = BTreeMap::new();
    let mut associated: BTreeMap<FactRef, BTreeSet<FactRef>> = BTreeMap::new();
    for document in structural {
        for record in records(document) {
            check_cancellation(cancellation)?;
            if let Some(previous) = index.insert(record.key(), record)
                && previous != record
            {
                return Err(FirstSliceError::Identity);
            }
            match record {
                Record::File(file) if !available.contains(&FactRef::File(file.id)) => {
                    return Err(FirstSliceError::Identity);
                }
                Record::Relation(relation) => {
                    for endpoint in [relation.subject, relation.object] {
                        if let RelationEndpoint::Occurrence(occurrence) = endpoint {
                            associated
                                .entry(FactRef::Fact(occurrence))
                                .or_default()
                                .insert(record.key());
                        }
                    }
                }
                Record::Extension(extension)
                    if matches!(
                        extension.namespace.as_str(),
                        SYMBOL_IDENTITY_CLAIM_NAMESPACE | LEXICAL_EXTENSION_NAMESPACE
                    ) =>
                {
                    if let [subject] = extension.evidence.derivation.as_slice() {
                        associated.entry(*subject).or_default().insert(record.key());
                    }
                }
                Record::Coverage(coverage)
                    if matches!(
                        coverage.domain,
                        FactDomain::Occurrences | FactDomain::Extensions | FactDomain::Diagnostics
                    ) && matches!(coverage.scope, CoverageScope::File(file) if affected_files.contains(&file)) =>
                {
                    // Every structural occurrence is retained or superseded at its exact
                    // source. Optional evidence coverage is adjusted to its retained subset.
                    pending.insert(record.key());
                }
                Record::Skipped(skipped)
                    if skipped.domain == FactDomain::Occurrences
                        && affected_files.contains(&skipped.source.span().file()) =>
                {
                    pending.insert(record.key());
                }
                Record::Diagnostic(diagnostic)
                    if diagnostic
                        .source
                        .as_ref()
                        .is_some_and(|source| affected_files.contains(&source.span().file())) =>
                {
                    pending.insert(record.key());
                }
                _ => {}
            }
        }
    }

    let mut retained = BTreeSet::new();
    while let Some(reference) = pending.pop_first() {
        check_cancellation(cancellation)?;
        // Existing project identities stop traversal: structural metadata must
        // not replace a stronger semantic record or its identity claim.
        if available.contains(&reference) || !retained.insert(reference) {
            continue;
        }
        let record = index.get(&reference).ok_or(FirstSliceError::Identity)?;
        record.dependencies(|dependency| {
            if !available.contains(&dependency) && !retained.contains(&dependency) {
                pending.insert(dependency);
            }
        });
        if let Some(dependents) = associated.get(&reference) {
            pending.extend(dependents);
        }
    }
    drop(sites);

    let mut counts = [
        project.files.len(),
        project.entities.len(),
        project.occurrences.len(),
        project.relations.len(),
        project.provenance.len(),
        project.source_mappings.len(),
        project.coverage_records.len(),
        project.skipped_regions.len(),
        project.diagnostics.len(),
        project.extensions.len(),
    ];
    let bounds = [
        (limits.max_files, FirstSliceResource::Files),
        (limits.max_entities, FirstSliceResource::Entities),
        (limits.max_occurrences, FirstSliceResource::Occurrences),
        (limits.max_relations, FirstSliceResource::Relations),
        (
            limits.max_provenance_records,
            FirstSliceResource::Provenance,
        ),
        (
            limits.max_source_mappings,
            FirstSliceResource::SourceMappings,
        ),
        (limits.max_coverage_records, FirstSliceResource::Coverage),
        (
            limits.max_skipped_regions,
            FirstSliceResource::SkippedRegions,
        ),
        (limits.max_diagnostics, FirstSliceResource::Diagnostics),
        (limits.max_extensions, FirstSliceResource::Extensions),
    ];
    checked_resource_length(
        normalized_record_count(&project)?,
        retained.len(),
        limits.max_total_records,
        FirstSliceResource::Records,
    )?;
    let mut extension_bytes = extension_payload_bytes(project.extensions.iter())?;
    for reference in &retained {
        check_cancellation(cancellation)?;
        let record = index.get(reference).ok_or(FirstSliceError::Identity)?;
        let slot = record.slot();
        let (maximum, resource) = bounds[slot];
        counts[slot] = checked_resource_length(counts[slot], 1, maximum, resource)?;
        if let Record::Extension(extension) = record {
            extension_bytes = checked_resource_length(
                extension_bytes,
                extension.payload.len(),
                limits.max_total_extension_bytes,
                FirstSliceResource::ExtensionBytes,
            )?;
        }
    }
    reserve(&mut project.files, counts[0])?;
    reserve(&mut project.entities, counts[1])?;
    reserve(&mut project.occurrences, counts[2])?;
    reserve(&mut project.relations, counts[3])?;
    reserve(&mut project.provenance, counts[4])?;
    reserve(&mut project.source_mappings, counts[5])?;
    reserve(&mut project.coverage_records, counts[6])?;
    reserve(&mut project.skipped_regions, counts[7])?;
    reserve(&mut project.diagnostics, counts[8])?;
    reserve(&mut project.extensions, counts[9])?;
    let mut optional_counts = BTreeMap::<(FactDomain, FileId, FactId), u64>::new();
    for record in records(&project).chain(retained.iter().filter_map(|key| index.get(key).copied()))
    {
        check_cancellation(cancellation)?;
        let group = match record {
            Record::Extension(extension) => extension.evidence.source.as_ref().map(|source| {
                (
                    FactDomain::Extensions,
                    source.span().file(),
                    extension.provenance,
                )
            }),
            Record::Diagnostic(diagnostic) => diagnostic.source.as_ref().map(|source| {
                (
                    FactDomain::Diagnostics,
                    source.span().file(),
                    diagnostic.provenance,
                )
            }),
            _ => None,
        };
        if let Some(group) = group {
            let count = optional_counts.entry(group).or_default();
            *count = count.checked_add(1).ok_or(FirstSliceError::Limits)?;
        }
    }
    for reference in retained {
        check_cancellation(cancellation)?;
        let record = index.get(&reference).ok_or(FirstSliceError::Identity)?;
        if let Record::Coverage(coverage) = record
            && matches!(
                coverage.domain,
                FactDomain::Extensions | FactDomain::Diagnostics
            )
            && let CoverageScope::File(file) = coverage.scope
        {
            // Structural claims and lexical metadata may be superseded individually.
            // Never charge their omissions to a different provider's file coverage.
            let mut coverage = (*coverage).clone();
            let indexed = optional_counts
                .get(&(coverage.domain, file, coverage.provenance))
                .copied()
                .unwrap_or(0);
            let omitted = coverage
                .indexed
                .checked_sub(indexed)
                .ok_or(FirstSliceError::Identity)?;
            coverage.indexed = indexed;
            coverage.skipped = coverage
                .skipped
                .checked_add(omitted)
                .ok_or(FirstSliceError::Limits)?;
            if omitted > 0 && coverage.status != CoverageStatus::Unknown {
                coverage.status = CoverageStatus::Bounded;
            }
            coverage.id =
                derive_coverage_record_id(&coverage).map_err(|_| FirstSliceError::Identity)?;
            project.coverage_records.push(coverage);
        } else {
            record.append(&mut project);
        }
    }
    Ok(project)
}

fn reserve<T>(records: &mut Vec<T>, total: usize) -> Result<(), FirstSliceError> {
    let additional = total
        .checked_sub(records.len())
        .ok_or(FirstSliceError::Limits)?;
    records
        .try_reserve_exact(additional)
        .map_err(|_| FirstSliceError::Limits)
}
