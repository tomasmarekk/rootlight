//! Literal GYP mentions refer to captured files without selecting build branches.

use super::*;
use rootlight_ir::{Confidence, EvidenceKind, FilePathLocator, OccurrenceTarget};

const RULE: &str = "gyp.literal-source-file/1";
const PRODUCER: &str = "rootlight-gyp-source-files";
const GAP: &str = "gyp-written-source-path-resolution-unavailable";

fn owned(record: &ProvenanceRecord) -> bool {
    record.producer.name() == PRODUCER && record.rule.as_deref() == Some(RULE)
}

fn exact_gap(gap: &SkippedRegion, source: &SourceRef) -> bool {
    gap.domain == IrFactDomain::Relations
        && gap.reason == SkippedRegionReason::UnsupportedConstruct
        && gap.detail == GAP
        && gap.source == *source
        && gap.evidence.source.as_ref() == Some(source)
}

fn retain_gap(
    plan: &mut NormalizedIrDocument,
    document: &NormalizedIrDocument,
    occurrence: &OccurrenceRecord,
    cancel: &Cancellation,
) -> Result<(), FirstSliceError> {
    for gap in &document.skipped_regions {
        check_cancellation(cancel)?;
        if exact_gap(gap, &occurrence.source) {
            return Ok(());
        }
    }
    let source = occurrence.source.clone();
    let mut gap = SkippedRegion {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        source: source.clone(),
        domain: IrFactDomain::Relations,
        reason: SkippedRegionReason::UnsupportedConstruct,
        detail: GAP.to_owned(),
        provenance: occurrence.provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: Vec::new(),
        },
    };
    gap.id = derive_skipped_region_id(&gap).map_err(|_| FirstSliceError::Identity)?;
    plan.skipped_regions
        .try_reserve(1)
        .map_err(|_| FirstSliceError::Limits)?;
    plan.skipped_regions.push(gap);
    Ok(())
}

fn authenticated<'a>(
    document: &NormalizedIrDocument,
    file: &FileRecord,
    snapshots: &BTreeMap<FileId, &'a SourceSnapshot>,
) -> Result<Option<(&'a SourceSnapshot, SourceRef)>, FirstSliceError> {
    let Some(snapshot) = snapshots.get(&file.id).copied() else {
        return Ok(None);
    };
    let Some(locator) = &file.path_locator else {
        return Ok(None);
    };
    let path = RelativePath::from_locator(locator).map_err(|_| FirstSliceError::Identity)?;
    let expected = rootlight_ids::derive_file(rootlight_ids::FileIdentity {
        repository: document.repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    let Some(source) = file.evidence.source.as_ref() else {
        return Ok(None);
    };
    if expected != file.id
        || snapshot.path().identity_bytes() != path.identity_bytes()
        || snapshot.file() != file.id
        || snapshot.content_hash() != file.content_hash
        || u64::try_from(snapshot.content().len()).ok() != Some(file.byte_length)
        || file.repository != document.repository
        || file.generation != document.generation
        || source.repository() != document.repository
        || source.generation() != document.generation
        || source.span().file() != file.id
        || source.content_hash() != file.content_hash
        || source.span().start_byte() != 0
        || source.span().end_byte() != file.byte_length
    {
        return Err(FirstSliceError::Identity);
    }
    Ok(Some((snapshot, source.clone())))
}

fn target_path(
    base: &RelativePath,
    raw: &str,
    cancel: &Cancellation,
) -> Result<Option<RelativePath>, FirstSliceError> {
    if raw.is_empty()
        || raw.len() > rootlight_vfs::MAX_PATH_BYTES
        || raw.starts_with('/')
        || raw.contains(['\\', ':', '\0', '<', '>', '$'])
        || raw.chars().any(char::is_control)
    {
        return Ok(None);
    }
    let locator = base.to_locator();
    let mut components = locator.components().to_vec();
    components.pop();
    for component in raw.split('/') {
        check_cancellation(cancel)?;
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Ok(None);
                }
            }
            component => {
                let Ok(part) = RelativePath::parse(Path::new(component)) else {
                    return Ok(None);
                };
                components.extend_from_slice(part.to_locator().components());
            }
        }
        if components.len() > rootlight_vfs::MAX_PATH_COMPONENTS {
            return Ok(None);
        }
    }
    let Ok(locator) = FilePathLocator::new(locator.encoding(), components) else {
        return Ok(None);
    };
    Ok(RelativePath::from_locator(&locator).ok())
}

pub(super) fn bind(
    document: &mut NormalizedIrDocument,
    sources: &[RustSourceInput],
    limits: &IrLimits,
    cancel: &Cancellation,
    admit_workspace: &mut impl FnMut(u64) -> Result<(), FirstSliceError>,
) -> Result<(), FirstSliceError> {
    let mut mentions = 0usize;
    let mut previous = 0usize;
    for occurrence in &document.occurrences {
        check_cancellation(cancel)?;
        mentions += usize::from(occurrence.syntax_kind == "gyp.source_path.reference");
    }
    for provenance in &document.provenance {
        check_cancellation(cancel)?;
        previous += usize::from(owned(provenance));
    }
    if mentions == 0 && previous == 0 {
        return Ok(());
    }
    // Reserve lookup nodes, candidate facts and one bounded locator workspace
    // before allocating. Existing final-workspace admission owns enforcement.
    let scratch = document
        .files
        .len()
        .checked_add(sources.len())
        .and_then(|n| n.checked_mul(128))
        .ok_or(FirstSliceError::Limits)?
        .checked_add(
            mentions
                .checked_add(previous)
                .and_then(|n| n.checked_mul(8192))
                .ok_or(FirstSliceError::Limits)?,
        )
        .and_then(|n| n.checked_add(rootlight_vfs::MAX_PATH_BYTES.checked_mul(8)?))
        .ok_or(FirstSliceError::Limits)?;
    let workspace = generation_neutral_workspace_bytes(document, || cancel.check().is_ok())
        .map_err(|_| current_cancellation(cancel).unwrap_or(FirstSliceError::Limits))?
        .checked_add(u64::try_from(scratch).map_err(|_| FirstSliceError::Limits)?)
        .ok_or(FirstSliceError::Limits)?;
    admit_workspace(workspace)?;
    let mut files = BTreeMap::new();
    for file in &document.files {
        check_cancellation(cancel)?;
        if files.insert(file.id, file).is_some() {
            return Err(FirstSliceError::Identity);
        }
    }
    let mut snapshots = BTreeMap::new();
    for source in sources {
        check_cancellation(cancel)?;
        if snapshots
            .insert(source.snapshot.file(), &source.snapshot)
            .is_some()
        {
            return Err(FirstSliceError::Identity);
        }
    }
    let mut old = BTreeSet::new();
    for provenance in &document.provenance {
        check_cancellation(cancel)?;
        if owned(provenance) {
            old.insert(provenance.id);
        }
    }
    let mut plan = NormalizedIrDocument::empty(document.repository, document.generation);
    plan.version = document.version;
    let mut bound = BTreeMap::new();
    for occurrence in &document.occurrences {
        check_cancellation(cancel)?;
        if occurrence.syntax_kind != "gyp.source_path.reference"
            || occurrence.role != OccurrenceRole::Reference
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
        {
            continue;
        }
        let Some(file) = files
            .get(&occurrence.file)
            .copied()
            .filter(|file| file.language == "gyp")
        else {
            return Err(FirstSliceError::Identity);
        };
        let Some((snapshot, origin)) = authenticated(document, file, &snapshots)? else {
            retain_gap(&mut plan, document, occurrence, cancel)?;
            continue;
        };
        let source = &occurrence.source;
        if source.repository() != document.repository
            || source.generation() != document.generation
            || source.span().file() != file.id
            || source.content_hash() != snapshot.content_hash()
            || occurrence.repository != document.repository
            || occurrence.generation != document.generation
        {
            return Err(FirstSliceError::Identity);
        }
        let start =
            usize::try_from(source.span().start_byte()).map_err(|_| FirstSliceError::Identity)?;
        let end =
            usize::try_from(source.span().end_byte()).map_err(|_| FirstSliceError::Identity)?;
        let raw = snapshot
            .content()
            .get(start..end)
            .ok_or(FirstSliceError::Identity)?;
        let quote = start.checked_sub(1).and_then(|n| snapshot.content().get(n));
        let literal = quote.is_some_and(|quote| {
            matches!(quote, b'\'' | b'"') && snapshot.content().get(end) == Some(quote)
        });
        let target = if literal && occurrence.syntactic_text_hash == content_hash(raw) {
            if let Ok(raw) = std::str::from_utf8(raw) {
                target_path(snapshot.path(), raw, cancel)?.map(|path| {
                    rootlight_ids::derive_file(rootlight_ids::FileIdentity {
                        repository: document.repository,
                        path_identity: path.identity_bytes(),
                    })
                    .id()
                })
            } else {
                None
            }
        } else {
            None
        };
        let target = target.and_then(|id| files.get(&id).copied());
        let proof = target
            .map(|target| {
                authenticated(document, target, &snapshots)
                    .map(|proof| proof.map(|(_, source)| (target.id, source)))
            })
            .transpose()?
            .flatten();
        let Some((target, target_source)) = proof else {
            retain_gap(&mut plan, document, occurrence, cancel)?;
            continue;
        };
        let mut parents = vec![
            FactRef::Fact(occurrence.id),
            FactRef::File(file.id),
            FactRef::File(target),
        ];
        parents.sort_unstable();
        parents.dedup();
        let mut inputs = vec![origin];
        if inputs[0] != target_source {
            inputs.push(target_source);
        }
        inputs.sort_unstable();
        inputs.dedup();
        let mut provenance = ProvenanceRecord {
            id: FactId::from_bytes([0; 20]),
            repository: document.repository,
            generation: document.generation,
            producer_kind: ProducerKind::Rule,
            producer: ProducerIdentity::new(PRODUCER, "1.0.0", first_slice_build_context())
                .map_err(|_| FirstSliceError::Identity)?,
            binary_digest: content_hash(RESOLVER_BINARY_SEED),
            frontend_version: Some(RULE.to_owned()),
            language: "gyp".to_owned(),
            tier: AnalysisTier::TierD,
            build_context: BuildContextIdentity::new(first_slice_build_context()),
            input_sources: inputs,
            evidence_sources: vec![source.clone()],
            derivation_parents: parents.clone(),
            rule: Some(RULE.to_owned()),
        };
        provenance.id =
            derive_provenance_record_id(&provenance).map_err(|_| FirstSliceError::Identity)?;
        let mut relation = RelationRecord {
            id: FactId::from_bytes([0; 20]),
            repository: document.repository,
            generation: document.generation,
            subject: RelationEndpoint::Occurrence(occurrence.id),
            predicate: RelationPredicate::RefersTo,
            object: RelationEndpoint::File(target),
            confidence: Confidence::new(1000).map_err(|_| FirstSliceError::Identity)?,
            evidence_kind: EvidenceKind::Syntax,
            provenance: provenance.id,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: parents,
            },
        };
        relation.id =
            derive_relation_record_id(&relation).map_err(|_| FirstSliceError::Identity)?;
        plan.relations
            .try_reserve(1)
            .map_err(|_| FirstSliceError::Limits)?;
        plan.provenance
            .try_reserve(1)
            .map_err(|_| FirstSliceError::Limits)?;
        plan.relations.push(relation);
        plan.provenance.push(provenance);
        bound.insert(source.span(), source.clone());
    }
    // IR quotas describe the final graph; workspace admission already covers
    // the simultaneous old records and replacement plan.
    let mut removed = NormalizedIrDocument::empty(document.repository, document.generation);
    removed.version = document.version;
    for relation in &document.relations {
        check_cancellation(cancel)?;
        if old.contains(&relation.provenance) {
            removed
                .relations
                .try_reserve(1)
                .map_err(|_| FirstSliceError::Limits)?;
            removed.relations.push(relation.clone());
        }
    }
    for provenance in &document.provenance {
        check_cancellation(cancel)?;
        if old.contains(&provenance.id) {
            removed
                .provenance
                .try_reserve(1)
                .map_err(|_| FirstSliceError::Limits)?;
            removed.provenance.push(provenance.clone());
        }
    }
    for gap in &document.skipped_regions {
        check_cancellation(cancel)?;
        if bound
            .get(&gap.source.span())
            .is_some_and(|source| exact_gap(gap, source))
        {
            removed
                .skipped_regions
                .try_reserve(1)
                .map_err(|_| FirstSliceError::Limits)?;
            removed.skipped_regions.push(gap.clone());
        }
    }
    // Complete checked final accounting and capacity before any mutation.
    measure_append_usage(document, limits, cancel)?
        .checked_sub(measure_append_usage(&removed, limits, cancel)?)
        .map_err(|error| map_ir_usage_error(error, cancel))?
        .checked_add(measure_append_usage(&plan, limits, cancel)?, limits)
        .map_err(|error| map_ir_usage_error(error, cancel))?;
    document
        .relations
        .try_reserve(plan.relations.len())
        .map_err(|_| FirstSliceError::Limits)?;
    document
        .provenance
        .try_reserve(plan.provenance.len())
        .map_err(|_| FirstSliceError::Limits)?;
    document
        .skipped_regions
        .try_reserve(plan.skipped_regions.len())
        .map_err(|_| FirstSliceError::Limits)?;
    check_cancellation(cancel)?;
    document
        .relations
        .retain(|relation| !old.contains(&relation.provenance));
    document
        .provenance
        .retain(|provenance| !old.contains(&provenance.id));
    document.relations.append(&mut plan.relations);
    document.provenance.append(&mut plan.provenance);
    document.skipped_regions.retain(|gap| {
        !(bound
            .get(&gap.source.span())
            .is_some_and(|source| exact_gap(gap, source)))
    });
    document.skipped_regions.append(&mut plan.skipped_regions);
    Ok(())
}
