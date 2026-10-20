//! Join bounded SCSS mixin certificates with exact current-frame IR identities.
//! The provider owns lexical admission; this consumer never resolves a name
//! without its non-container target-start to call-end source witness.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, FactRef, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "scss.mixin_target.signature" | "scss.parameter_list_unavailable.signature"
        )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = if fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "scss.parameter_list_unavailable.signature"
    {
        &[
            (
                FactDomain::Entities,
                "scss-complex-formal-declaration-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "scss-complex-formal-definition-unavailable",
            ),
            (
                FactDomain::Relations,
                "scss-complex-formal-owner-unavailable",
            ),
        ]
    } else {
        &[]
    };
    rows.iter().copied()
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|previous| *previous = None)
        .or_insert(Some(value));
}

fn same_name(left: &str, right: &str, cancel: &Cancellation) -> Result<bool, AdapterError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.bytes().zip(right.bytes()) {
        cancel.check()?;
        if left != right && !(matches!(left, b'_' | b'-') && matches!(right, b'_' | b'-')) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn call_ends(
    facts: &[SyntaxFact],
    cancel: &Cancellation,
) -> Result<HashMap<(rootlight_ids::FileId, u64), ()>, AdapterError> {
    let mut ends = HashMap::new();
    for fact in facts {
        cancel.check()?;
        if fact.kind() == SyntaxFactKind::Signature
            && fact.syntax_kind().as_str() == "scss.mixin_target.signature"
        {
            ends.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            ends.insert((fact.span().file(), fact.span().end_byte()), ());
        }
    }
    Ok(ends)
}

struct Binding {
    target: SymbolId,
    owner: SymbolId,
    span: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
}

pub(super) struct CallTargets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl CallTargets {
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut all = HashMap::new();
        let mut declarations = HashMap::new();
        let mut definitions = HashMap::new();
        let mut calls = HashMap::new();
        let mut markers = HashMap::new();
        for map in [
            &mut all,
            &mut declarations,
            &mut definitions,
            &mut calls,
            &mut markers,
        ] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        let mut roots = 0;
        let mut root = None;
        let mut recovered = false;
        for fact in facts {
            cancel.check()?;
            unique(&mut all, fact.local_id(), fact);
            recovered |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            if fact.kind() == SyntaxFactKind::Root
                && fact.syntax_kind().as_str() == "scss.source_file"
                && physical(fact.span())? == source.span()
            {
                roots += 1;
                root = Some(fact.local_id());
            }
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Declaration, "scss.mixin.declaration") => {
                    unique(&mut declarations, physical(fact.span())?.start_byte(), fact)
                }
                (SyntaxFactKind::Occurrence, "scss.identifier.definition") => {
                    if let Some(parent) = fact.parent() {
                        unique(&mut definitions, parent, fact);
                    }
                }
                (SyntaxFactKind::Occurrence, "scss.mixin.call") => {
                    unique(&mut calls, physical(fact.span())?.end_byte(), fact)
                }
                (SyntaxFactKind::Signature, "scss.mixin_target.signature") => {
                    unique(&mut markers, physical(fact.span())?.end_byte(), fact)
                }
                _ => {}
            }
        }
        let mut bindings = HashMap::new();
        bindings
            .try_reserve(markers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        if roots != 1 || recovered {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        }
        for marker in markers.values().copied().flatten() {
            cancel.check()?;
            let witness = physical(marker.span())?;
            let Some(target_fact) = declarations.get(&witness.start_byte()).copied().flatten()
            else {
                continue;
            };
            let Some(target) = entities.get(&target_fact.local_id()) else {
                continue;
            };
            let Some(definition) = definitions.get(&target_fact.local_id()).copied().flatten()
            else {
                continue;
            };
            let Some(call) = calls.get(&witness.end_byte()).copied().flatten() else {
                continue;
            };
            let Some(scope) = call
                .parent()
                .and_then(|local| all.get(&local).copied().flatten())
            else {
                continue;
            };
            let Some(owner_fact) = scope
                .parent()
                .and_then(|local| all.get(&local).copied().flatten())
            else {
                continue;
            };
            let Some(owner) = entities.get(&owner_fact.local_id()) else {
                continue;
            };
            let Some(target_source) = target.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(owner_source) = owner.record.evidence.source.as_ref() else {
                continue;
            };
            let span = physical(call.span())?;
            let name = text(definition.span())?;
            if marker.parent() != root
                || target_fact.parent() != root
                || owner_fact.parent() != root
                || owner_fact.kind() != SyntaxFactKind::Declaration
                || owner_fact.syntax_kind().as_str() != "scss.mixin.declaration"
                || scope.kind() != SyntaxFactKind::Scope
                || scope.syntax_kind().as_str() != "scss.block.scope"
                || scope.parent() != Some(owner_fact.local_id())
                || target.record.kind != EntityKind::Function
                || owner.record.kind != EntityKind::Function
                || target.record.language != "scss"
                || owner.record.language != "scss"
                || !same_source(target_source, source)
                || !same_source(owner_source, source)
                || target.record.repository != source.repository()
                || owner.record.repository != source.repository()
                || target.record.generation != source.generation()
                || owner.record.generation != source.generation()
                || target.direct_parent != ContainerRef::File(source.span().file())
                || owner.direct_parent != ContainerRef::File(source.span().file())
                || target.record.container != Some(target.direct_parent)
                || owner.record.container != Some(owner.direct_parent)
                || target_source.span() != physical(target_fact.span())?
                || owner_source.span() != physical(owner_fact.span())?
                || target.definition_local_id != Some(definition.local_id())
                || target.record.canonical_name != name
                || !same_name(name, text(call.span())?, cancel)?
                || physical(definition.span())?.start_byte() < target_source.span().start_byte()
                || physical(definition.span())?.end_byte() > target_source.span().end_byte()
                || physical(scope.span())?.start_byte() < owner_source.span().start_byte()
                || physical(scope.span())?.end_byte() != owner_source.span().end_byte()
                || target_source.span().end_byte() > owner_source.span().start_byte()
                || span.start_byte() < physical(scope.span())?.start_byte()
                || span.end_byte() > owner_source.span().end_byte()
                || witness.start_byte() != target_source.span().start_byte()
                || witness.end_byte() != span.end_byte()
                || target.record.provenance != owner.record.provenance
                || span.file() != source.span().file()
            {
                continue;
            }
            bindings.insert(
                call.local_id(),
                Binding {
                    target: target.record.id,
                    owner: owner.record.id,
                    span,
                    spelling: content_hash(text(call.span())?.as_bytes()),
                    provenance: owner.record.provenance,
                },
            );
        }
        Ok(Self {
            source: source.clone(),
            bindings,
        })
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        site: &OccurrenceRecord,
        physical_span: SourceSpan,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "scss.mixin.call"
            || site.syntax_kind != "scss.mixin.call"
            || site.role != OccurrenceRole::CallSite
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
            || !same_source(&site.source, &self.source)
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
        {
            return Ok(None);
        }
        Ok(self
            .bindings
            .get(&fact.local_id())
            .filter(|binding| {
                binding.span == physical_span
                    && binding.span == site.source.span()
                    && binding.spelling == site.syntactic_text_hash
                    && site.enclosing == Some(binding.owner)
                    && binding.provenance == site.provenance
            })
            .map(|binding| binding.target))
    }
}

pub(super) fn call_relation(
    site: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    target: SymbolId,
    cancel: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancel.check()?;
    let Some(owner) = owner.filter(|owner| {
        owner.record.language == "scss"
            && owner.record.kind == EntityKind::Function
            && owner
                .record
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| same_source(source, &site.source))
    }) else {
        return Ok(None);
    };
    if site.syntax_kind != "scss.mixin.call"
        || site.role != OccurrenceRole::CallSite
        || site.enclosing != Some(owner.record.id)
        || site.target != (OccurrenceTarget::Resolved { symbol: target })
    {
        return Ok(None);
    }
    let mut relation = lexical_reference_relation(site, target)?;
    relation.subject = RelationEndpoint::Entity(owner.record.id);
    relation.predicate = RelationPredicate::Calls;
    relation
        .evidence
        .derivation
        .try_reserve_exact(3)
        .map_err(|_| SinkError::AllocationFailed)?;
    relation.evidence.derivation.extend([
        FactRef::Entity(owner.record.id),
        FactRef::Fact(site.id),
        FactRef::Entity(target),
    ]);
    relation.evidence.derivation.sort_unstable();
    relation.evidence.derivation.dedup();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("scss-direct-mixin-call-identity"))?;
    Ok(Some(relation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lowering::*;
    use rootlight_adapter_sdk::{
        BatchThresholds, CoverageReport, DomainCoverage, EncodingId, GenerationBoundSnapshot,
        LanguageId, MemoryEnforcement, ParseCapabilities, ParseReport, ParseRequest, ResourceUsage,
        StreamEnd, StreamLimits, SyntaxFactBatch, SyntaxFactSink, SyntaxKindLabel,
    };
    use rootlight_ids::{GenerationId, derive_repository};
    use rootlight_vfs::{RelativePath, RepositoryRoot};
    use std::{fs, path::Path};

    // This replay fixes the preflight input independently from the producer.
    // Public adapter tests separately require that actual SCSS emits the proof.
    struct BoundedProof {
        capabilities: ParseCapabilities,
        facts: Vec<SyntaxFact>,
    }

    impl BoundedProof {
        fn new(file: rootlight_ids::FileId, source: &str, marker: bool) -> Self {
            let caller = source.find("@mixin caller").unwrap();
            let first_end = source.find('}').unwrap() + 1;
            let target_name = source.find("target").unwrap();
            let caller_name = source.find("caller").unwrap();
            let call_name = source.rfind("target").unwrap();
            let mut facts = Vec::new();
            let mut push = |parent, kind, start, end, depth, label| {
                facts.push(SyntaxFact::new(
                    facts.len() as u64 + 1,
                    parent,
                    kind,
                    SourceSpan::new(file, start as u64, end as u64).unwrap(),
                    depth,
                    SyntaxKindLabel::new(label).unwrap(),
                ));
            };
            push(
                None,
                SyntaxFactKind::Root,
                0,
                source.len(),
                0,
                "scss.source_file",
            );
            push(
                Some(1),
                SyntaxFactKind::Declaration,
                0,
                first_end,
                1,
                "scss.mixin.declaration",
            );
            push(
                Some(2),
                SyntaxFactKind::Signature,
                target_name,
                target_name + 6,
                2,
                "scss.name.signature",
            );
            push(
                Some(2),
                SyntaxFactKind::Occurrence,
                target_name,
                target_name + 6,
                2,
                "scss.identifier.definition",
            );
            push(
                Some(2),
                SyntaxFactKind::Scope,
                source.find('{').unwrap(),
                first_end,
                2,
                "scss.block.scope",
            );
            push(
                Some(1),
                SyntaxFactKind::Declaration,
                caller,
                source.len(),
                1,
                "scss.mixin.declaration",
            );
            push(
                Some(6),
                SyntaxFactKind::Signature,
                caller_name,
                caller_name + 6,
                2,
                "scss.name.signature",
            );
            push(
                Some(6),
                SyntaxFactKind::Occurrence,
                caller_name,
                caller_name + 6,
                2,
                "scss.identifier.definition",
            );
            push(
                Some(6),
                SyntaxFactKind::Scope,
                caller + source[caller..].find('{').unwrap(),
                source.len(),
                2,
                "scss.block.scope",
            );
            push(
                Some(9),
                SyntaxFactKind::Occurrence,
                call_name,
                call_name + 6,
                3,
                "scss.mixin.call",
            );
            if marker {
                push(
                    Some(1),
                    SyntaxFactKind::Signature,
                    0,
                    call_name + 6,
                    1,
                    "scss.mixin_target.signature",
                );
            }
            Self {
                capabilities: ParseCapabilities::new(
                    vec![LanguageId::new("scss").unwrap()],
                    vec![EncodingId::utf8()],
                    4096,
                    128,
                    16,
                    0,
                    false,
                    false,
                    true,
                    1,
                    MemoryEnforcement::Unavailable,
                )
                .unwrap(),
                facts,
            }
        }
    }

    impl ParseProvider for BoundedProof {
        fn capabilities(&self) -> &ParseCapabilities {
            &self.capabilities
        }
        fn parse(
            &self,
            request: &ParseRequest<'_>,
            sink: &mut dyn SyntaxFactSink,
            cancel: &Cancellation,
        ) -> Result<ParseReport, AdapterError> {
            cancel.check()?;
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), self.facts.clone(), Vec::new()),
                cancel,
            )?;
            let size = request.source().bytes().len();
            let coverage = CoverageReport::new(
                AnalysisTier::TierD,
                CoverageStatus::Bounded,
                size,
                size,
                0,
                vec![DomainCoverage::new(
                    FactDomain::Files,
                    CoverageStatus::Complete,
                    1,
                    1,
                    0,
                )?],
            )?;
            let usage = sink.staged_usage();
            Ok(ParseReport::new(
                coverage,
                ResourceUsage::new(size, self.facts.len(), self.facts.len(), 3, None, usage),
                StreamEnd::new(sink.next_sequence(), usage),
            )?)
        }
    }

    fn limits(ir: IrLimits) -> AnalysisLimits {
        let stream = StreamLimits::new(
            128,
            4096,
            1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(64, 128 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap();
        AnalysisLimits::new(4096, 128, 16, 1, 1024 * 1024, stream.clone(), stream, ir).unwrap()
    }

    fn request<'a>(
        snapshot: &'a rootlight_vfs::SourceSnapshot,
        reference: &SourceRef,
        limits: &'a AnalysisLimits,
    ) -> AnalysisRequest<'a> {
        AnalysisRequest::new(
            GenerationBoundSnapshot::new(snapshot, reference).unwrap(),
            LanguageId::new("scss").unwrap(),
            AnalysisTier::TierD,
            rootlight_ir::BuildContextIdentity::new(content_hash(b"scss-capacity")),
            limits,
        )
        .unwrap()
        .with_generated_status(false)
    }

    #[test]
    fn bounded_mixin_markers_reserve_calls_and_three_proof_parents_before_lowering() {
        let source = "@mixin target() {} @mixin caller() { @include target(); }";
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("sample.scss"), source).unwrap();
        let repository = derive_repository(b"scss-source-capacity").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("sample.scss")).unwrap(),
                4096,
            )
            .unwrap();
        let reference = SourceRef::new(
            repository,
            GenerationId::from_bytes([18; 20]),
            SourceSpan::new(snapshot.file(), 0, source.len() as u64).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let base_limits = limits(IrLimits::default());
        let base_request = request(&snapshot, &reference, &base_limits);
        let cancel = Cancellation::new();
        let producer =
            ProducerIdentity::new("scss-source-capacity", "1.0", content_hash(b"capacity"))
                .unwrap();
        let mut budgets = Vec::new();
        let mut parsed_marker = None;
        for marker in [false, true] {
            let provider = BoundedProof::new(snapshot.file(), source, marker);
            let parsed = rootlight_adapter_sdk::execute_parse(
                &provider,
                &base_request.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
            .unwrap();
            assert_eq!(parsed.report().coverage().status(), CoverageStatus::Bounded);
            validate_fact_graph(parsed.facts(), &[], &reference, &cancel).unwrap();
            budgets.push(
                preflight_lowering_limits(
                    &base_request,
                    &parsed,
                    &producer,
                    "source-capacity",
                    &[],
                    &cancel,
                )
                .unwrap(),
            );
            if marker {
                parsed_marker = Some(parsed);
            }
        }
        assert_eq!(budgets[1].records, budgets[0].records + 1);
        assert_eq!(budgets[1].nested_items, budgets[0].nested_items + 3);
        assert_eq!(budgets[1].nested_items, 5);
        let parsed = parsed_marker.unwrap();
        let mut exact = IrLimits::default();
        exact.max_relations = 3;
        exact.max_total_nested_items = 5;
        exact.max_nested_items_per_record = 3;
        let exact_limits = limits(exact.clone());
        preflight_lowering_limits(
            &request(&snapshot, &reference, &exact_limits),
            &parsed,
            &producer,
            "source-capacity",
            &[],
            &cancel,
        )
        .unwrap();
        for boundary in 0..3 {
            let mut low = exact.clone();
            let (resource, observed, limit) = match boundary {
                0 => {
                    low.max_relations = 2;
                    (ResourceKind::Records, 3, 2)
                }
                1 => {
                    low.max_total_nested_items = 4;
                    (ResourceKind::NestedItems, 5, 4)
                }
                _ => {
                    low.max_nested_items_per_record = 2;
                    (ResourceKind::NestedItems, 3, 2)
                }
            };
            let low_limits = limits(low);
            assert!(
                matches!(preflight_lowering_limits(&request(&snapshot, &reference, &low_limits), &parsed, &producer, "source-capacity", &[], &cancel),
                Err(AdapterError::Sink(SinkError::StreamLimit { resource: actual, observed: count, limit: ceiling }))
                if actual == resource && count == observed && ceiling == limit)
            );
        }
    }
}
