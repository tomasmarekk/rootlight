//! Join native same-class Apex call proofs to source-owned method identities.
//! A certified virtual call selects a static declaration independently of
//! its bounded runtime candidates and unavailable project override closure.

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
};

use super::*;

#[derive(Clone, Copy)]
struct FoldedName<'a>(&'a str);

impl PartialEq for FoldedName<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(other.0)
    }
}
impl Eq for FoldedName<'_> {}
impl Hash for FoldedName<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for byte in self.0.bytes() {
            byte.to_ascii_lowercase().hash(state);
        }
    }
}

pub(super) struct CallTargets<'a> {
    methods: HashMap<(SymbolId, FoldedName<'a>), Option<SymbolId>>,
}

impl<'a> CallTargets<'a> {
    /// Indexes only methods directly owned by a class in this immutable source.
    /// Cancellation and fallible allocation errors propagate before publication.
    pub(super) fn new(
        materialized: &'a HashMap<u64, MaterializedEntity>,
        full_source: &SourceRef,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut classes = HashSet::new();
        classes
            .try_reserve(materialized.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let valid_source = |record: &EntityRecord| {
            record.language == "apex"
                && record.evidence.source.as_ref().is_some_and(|reference| {
                    reference.repository() == full_source.repository()
                        && reference.generation() == full_source.generation()
                        && reference.span().file() == full_source.span().file()
                        && reference.content_hash() == full_source.content_hash()
                        && span_contains(full_source.span(), reference.span())
                })
        };
        for entity in materialized.values() {
            cancellation.check()?;
            if entity.record.kind == EntityKind::Class && valid_source(&entity.record) {
                classes.insert(entity.record.id);
            }
        }
        let mut methods = HashMap::new();
        methods
            .try_reserve(materialized.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in materialized.values() {
            cancellation.check()?;
            let record = &entity.record;
            if record.kind != EntityKind::Method
                || !valid_source(record)
                || !record.canonical_name.is_ascii()
            {
                continue;
            }
            let ContainerRef::Entity(class) = entity.direct_parent else {
                continue;
            };
            if !classes.contains(&class) {
                continue;
            }
            methods
                .entry((class, FoldedName(&record.canonical_name)))
                .and_modify(|target| *target = None)
                .or_insert(Some(record.id));
        }
        Ok(Self { methods })
    }

    /// Resolves a native-certified name only in its exact source-owned class.
    /// Missing, non-ASCII, ambiguous and foreign owners decline the proof.
    pub(super) fn resolve(
        &self,
        name: &str,
        owner: Option<&MaterializedEntity>,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        let Some(owner) = owner.filter(|owner| {
            owner.record.language == "apex" && owner.record.kind == EntityKind::Method
        }) else {
            return Ok(None);
        };
        let ContainerRef::Entity(class) = owner.direct_parent else {
            return Ok(None);
        };
        if !name.is_ascii() {
            return Ok(None);
        }
        Ok(self
            .methods
            .get(&(class, FoldedName(name)))
            .copied()
            .flatten())
    }
}

/// Applies certified static binding while preserving bounded virtual dispatch.
/// The additional virtual Calls edge names the statically selected declaration;
/// the occurrence and dispatch edge never claim a unique runtime implementation.
pub(super) fn apply(
    occurrence: &mut OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    symbol: SymbolId,
    cancellation: &Cancellation,
) -> Result<Option<(RelationRecord, Option<RelationRecord>)>, AdapterError> {
    cancellation.check()?;
    let Some(owner) = owner
        .filter(|owner| owner.record.language == "apex" && owner.record.kind == EntityKind::Method)
    else {
        return Ok(None);
    };
    if occurrence.role != OccurrenceRole::CallSite || occurrence.enclosing != Some(owner.record.id)
    {
        return Ok(None);
    }
    let private = occurrence.syntax_kind == "apex.private_instance_call.call";
    let virtual_base = occurrence.syntax_kind == "apex.virtual_instance_call.call";
    if !private && !virtual_base {
        return Ok(None);
    }
    occurrence.target = if private {
        OccurrenceTarget::Resolved { symbol }
    } else {
        let mut symbols = Vec::new();
        symbols
            .try_reserve_exact(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        symbols.push(symbol);
        OccurrenceTarget::Candidates {
            symbols,
            total_count: 1,
            completeness: CoverageStatus::Bounded,
        }
    };
    occurrence.id = derive_occurrence_record_id(occurrence)
        .map_err(|_| provider_failure("apex-written-call-identity"))?;
    let mut relation = lexical_reference_relation(occurrence, symbol)?;
    relation.predicate = if private {
        RelationPredicate::Calls
    } else {
        RelationPredicate::DispatchCandidate
    };
    if private {
        relation.subject = RelationEndpoint::Entity(owner.record.id);
    }
    relation
        .evidence
        .derivation
        .try_reserve_exact(if private { 3 } else { 2 })
        .map_err(|_| SinkError::AllocationFailed)?;
    relation
        .evidence
        .derivation
        .extend([FactRef::Fact(occurrence.id), FactRef::Entity(symbol)]);
    if private {
        relation
            .evidence
            .derivation
            .push(FactRef::Entity(owner.record.id));
    }
    relation.evidence.derivation.sort_unstable();
    relation.evidence.derivation.dedup();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("apex-written-call-relation-identity"))?;
    let declared_call = if virtual_base {
        let mut declared = lexical_reference_relation(occurrence, symbol)?;
        declared.subject = RelationEndpoint::Entity(owner.record.id);
        declared.predicate = RelationPredicate::Calls;
        declared
            .evidence
            .derivation
            .try_reserve_exact(3)
            .map_err(|_| SinkError::AllocationFailed)?;
        declared.evidence.derivation.extend([
            FactRef::Entity(owner.record.id),
            FactRef::Fact(occurrence.id),
            FactRef::Entity(symbol),
        ]);
        declared.evidence.derivation.sort_unstable();
        declared.evidence.derivation.dedup();
        declared.id = derive_relation_record_id(&declared)
            .map_err(|_| provider_failure("apex-declared-call-relation-identity"))?;
        Some(declared)
    } else {
        None
    };
    cancellation.check()?;
    Ok(Some((relation, declared_call)))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, sync::Arc};

    use crate::lowering::*;
    use rootlight_adapter_sdk::{
        BatchThresholds, GenerationBoundSnapshot, LanguageId, StreamLimits,
    };
    use rootlight_ids::{GenerationId, derive_repository};
    use rootlight_ir::BuildContextIdentity;
    use rootlight_vfs::{RelativePath, RepositoryRoot};

    fn limits(ir: IrLimits) -> AnalysisLimits {
        let stream = StreamLimits::new(
            128,
            16_384,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            4 * 1024 * 1024,
            BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).unwrap(),
        )
        .unwrap();
        AnalysisLimits::new(
            1024 * 1024,
            16_384,
            128,
            32,
            16 * 1024 * 1024,
            stream.clone(),
            stream,
            ir,
        )
        .unwrap()
    }

    #[test]
    fn private_apex_call_preflight_reserves_relation_and_three_proof_parents() {
        let source =
            "public class Handler { public void run() { target(); } private void target() {} }";
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("Handler.cls"), source).unwrap();
        let repository = derive_repository(b"apex-call-capacity").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("Handler.cls")).unwrap(),
                1024 * 1024,
            )
            .unwrap();
        let source_ref = SourceRef::new(
            repository,
            GenerationId::from_bytes([9; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(source.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let provider = crate::TreeSitterProvider::new(
            crate::RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                32,
                64,
                1,
                16 * 1024 * 1024,
                crate::ParserSettings::new(4096).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let producer =
            ProducerIdentity::new("apex-call-capacity", "1.0", content_hash(b"test")).unwrap();
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(provider),
            producer.clone(),
            LanguageId::new("apex").unwrap(),
            "native-module-capacity",
            content_hash(b"capacity-binary"),
        )
        .unwrap();
        let parse_limits = limits(IrLimits::default());
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
            LanguageId::new("apex").unwrap(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"capacity-build")),
            &parse_limits,
        )
        .unwrap()
        .with_generated_status(false);
        let cancellation = Cancellation::new();
        let parsed = analyzer.parse_request(&request, &cancellation).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "apex.private_instance_call.call")
                .count(),
            1
        );
        let entity_count = parsed
            .facts()
            .iter()
            .filter(|fact| structural_entity_kind(fact).is_some())
            .count();
        for per_record in [true, false] {
            let mut ir = IrLimits::default();
            if per_record {
                ir.max_nested_items_per_record = 2;
            } else {
                ir.max_relations = entity_count;
            }
            let budget = limits(ir);
            let request = AnalysisRequest::new(
                GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
                LanguageId::new("apex").unwrap(),
                AnalysisTier::TierD,
                BuildContextIdentity::new(content_hash(b"capacity-build")),
                &budget,
            )
            .unwrap()
            .with_generated_status(false);
            let rejected = preflight_lowering_limits(
                &request,
                &parsed,
                &producer,
                "native-module-capacity",
                &[],
                &cancellation,
            );
            if per_record {
                assert!(matches!(
                    rejected,
                    Err(AdapterError::Sink(SinkError::StreamLimit {
                        resource: ResourceKind::NestedItems,
                        observed: 3,
                        limit: 2
                    }))
                ));
            } else {
                assert!(
                    matches!(rejected, Err(AdapterError::Sink(SinkError::StreamLimit {resource: ResourceKind::Records, observed, limit})) if observed == entity_count + 1 && limit == entity_count)
                );
            }
        }
    }

    #[test]
    fn virtual_apex_call_preflight_reserves_static_and_dispatch_relations() {
        let source = "public virtual class Handler { public void run() { this.target(); } protected virtual void target() {} }";
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("Handler.cls"), source).unwrap();
        let repository = derive_repository(b"apex-call-capacity").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("Handler.cls")).unwrap(),
                1024 * 1024,
            )
            .unwrap();
        let source_ref = SourceRef::new(
            repository,
            GenerationId::from_bytes([9; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(source.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let provider = crate::TreeSitterProvider::new(
            crate::RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                32,
                64,
                1,
                16 * 1024 * 1024,
                crate::ParserSettings::new(4096).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let producer =
            ProducerIdentity::new("apex-call-capacity", "1.0", content_hash(b"test")).unwrap();
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(provider),
            producer.clone(),
            LanguageId::new("apex").unwrap(),
            "native-module-capacity",
            content_hash(b"capacity-binary"),
        )
        .unwrap();
        let parse_limits = limits(IrLimits::default());
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
            LanguageId::new("apex").unwrap(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"capacity-build")),
            &parse_limits,
        )
        .unwrap()
        .with_generated_status(false);
        let cancellation = Cancellation::new();
        let parsed = analyzer.parse_request(&request, &cancellation).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "apex.virtual_instance_call.call")
                .count(),
            1
        );
        let entity_count = parsed
            .facts()
            .iter()
            .filter(|fact| structural_entity_kind(fact).is_some())
            .count();
        for per_record in [true, false] {
            let mut ir = IrLimits::default();
            if per_record {
                ir.max_nested_items_per_record = 2;
            } else {
                ir.max_relations = entity_count + 1;
            }
            let budget = limits(ir);
            let request = AnalysisRequest::new(
                GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
                LanguageId::new("apex").unwrap(),
                AnalysisTier::TierD,
                BuildContextIdentity::new(content_hash(b"capacity-build")),
                &budget,
            )
            .unwrap()
            .with_generated_status(false);
            let rejected = preflight_lowering_limits(
                &request,
                &parsed,
                &producer,
                "native-module-capacity",
                &[],
                &cancellation,
            );
            if per_record {
                assert!(matches!(
                    rejected,
                    Err(AdapterError::Sink(SinkError::StreamLimit {
                        resource: ResourceKind::NestedItems,
                        observed: 3,
                        limit: 2
                    }))
                ));
            } else {
                assert!(
                    matches!(rejected, Err(AdapterError::Sink(SinkError::StreamLimit {resource: ResourceKind::Records, observed, limit})) if observed == entity_count + 2 && limit == entity_count + 1)
                );
            }
        }
    }

    fn checked_virtual_capacity_output(
        output: &AnalysisOutput,
        source: &str,
        full_source: &SourceRef,
    ) -> FactId {
        let document = output.document();
        rootlight_ir::validate_ir_document(
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        assert_eq!(document.files.len(), 1);
        assert_eq!(document.files[0].id, full_source.span().file());
        assert_eq!(
            document.files[0].content_hash,
            content_hash(source.as_bytes())
        );
        assert_eq!(
            document.files[0].byte_length,
            u64::try_from(source.len()).unwrap()
        );
        let named = |name: &str| {
            let matches: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Method && entity.canonical_name == name)
                .collect();
            assert_eq!(matches.len(), 1, "{name}");
            matches[0]
        };
        let caller = named("run");
        let target = named("target");
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "apex.virtual_instance_call.call")
            .collect();
        assert_eq!(sites.len(), 1);
        let site = sites[0];
        assert_eq!(site.role, OccurrenceRole::CallSite);
        assert_eq!(site.enclosing, Some(caller.id));
        let call = "this.target()";
        let start = source.find(call).unwrap();
        assert_eq!(
            site.source.span().start_byte(),
            u64::try_from(start).unwrap()
        );
        assert_eq!(
            site.source.span().end_byte(),
            u64::try_from(start + call.len()).unwrap()
        );
        assert_eq!(
            &source.as_bytes()[start..start + call.len()],
            b"this.target()"
        );
        assert_eq!(site.source.repository(), full_source.repository());
        assert_eq!(site.source.generation(), full_source.generation());
        assert_eq!(site.source.span().file(), full_source.span().file());
        assert_eq!(site.source.content_hash(), full_source.content_hash());
        assert_eq!(site.syntactic_text_hash, content_hash(b"target"));
        assert_eq!(
            site.target,
            OccurrenceTarget::Candidates {
                symbols: vec![target.id],
                total_count: 1,
                completeness: CoverageStatus::Bounded,
            }
        );
        let static_edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Entity(caller.id)
                    && edge.object == RelationEndpoint::Entity(target.id)
            })
            .collect();
        let dispatch_edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::DispatchCandidate
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(target.id)
            })
            .collect();
        assert_eq!(static_edges.len(), 1);
        assert_eq!(dispatch_edges.len(), 1);
        let mut static_proof = [
            FactRef::Entity(caller.id),
            FactRef::Fact(site.id),
            FactRef::Entity(target.id),
        ];
        static_proof.sort_unstable();
        assert_eq!(static_edges[0].evidence.derivation, static_proof);
        let mut dispatch_proof = [FactRef::Fact(site.id), FactRef::Entity(target.id)];
        dispatch_proof.sort_unstable();
        assert_eq!(dispatch_edges[0].evidence.derivation, dispatch_proof);
        for edge in [static_edges[0], dispatch_edges[0]] {
            assert_eq!(edge.evidence.source.as_ref(), Some(&site.source));
            assert_eq!(edge.confidence, site.confidence);
            assert_eq!(edge.evidence_kind, EvidenceKind::Derived);
            assert_eq!(edge.provenance, site.provenance);
            assert_eq!(edge.id, derive_relation_record_id(edge).unwrap());
        }
        let provenance = document
            .provenance
            .iter()
            .find(|item| item.id == site.provenance)
            .unwrap();
        assert_eq!(provenance.language, "apex");
        assert_eq!(provenance.repository, full_source.repository());
        assert_eq!(provenance.generation, full_source.generation());
        assert!(provenance.input_sources.contains(full_source));
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.domain == FactDomain::Relations
                && gap.detail == "apex-runtime-receiver-and-project-overrides-unavailable"
                && gap.source.repository() == full_source.repository()
                && gap.source.generation() == full_source.generation()
                && gap.source.content_hash() == full_source.content_hash()
                && gap.source.span().file() == full_source.span().file()
                && gap.source.span().start_byte() <= site.source.span().start_byte()
                && site.source.span().end_byte() <= gap.source.span().end_byte()
        }));
        assert_ne!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        static_edges[0].id
    }

    fn virtual_apex_aggregate_capacity_boundary(record_bound: bool) {
        let source = "public virtual class Handler { public void run() { this.target(); } protected virtual void target() {} }";
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("Handler.cls"), source).unwrap();
        let repository = derive_repository(b"apex-aggregate-call-capacity").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("Handler.cls")).unwrap(),
                1024 * 1024,
            )
            .unwrap();
        let source_ref = SourceRef::new(
            repository,
            GenerationId::from_bytes([9; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(source.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let provider = crate::TreeSitterProvider::new(
            crate::RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                32,
                64,
                1,
                16 * 1024 * 1024,
                crate::ParserSettings::new(4096).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let producer =
            ProducerIdentity::new("apex-call-capacity", "1.0", content_hash(b"test")).unwrap();
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(provider),
            producer,
            LanguageId::new("apex").unwrap(),
            "native-module-capacity",
            content_hash(b"capacity-binary"),
        )
        .unwrap();
        let parse_limits = limits(IrLimits::default());
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
            LanguageId::new("apex").unwrap(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"capacity-build")),
            &parse_limits,
        )
        .unwrap()
        .with_generated_status(false);
        let cancellation = Cancellation::new();
        let parsed = analyzer.parse_request(&request, &cancellation).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "apex.virtual_instance_call.call")
                .count(),
            1
        );
        let prepared = PreparsedTreeSitterAnalyzer {
            analyzer: &analyzer,
            parse_output: &parsed,
        };
        let baseline = execute_analysis(
            &prepared,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
        let static_edge = checked_virtual_capacity_output(&baseline, source, &source_ref);
        // The independent IR census measures the added relation and its actual
        // proof vector; it does not repeat the conservative preflight formula.
        let complete =
            rootlight_ir::IrDocumentUsage::measure(baseline.document(), &IrLimits::default())
                .unwrap();
        let mut without_static = baseline.document().clone();
        without_static
            .relations
            .retain(|edge| edge.id != static_edge);
        assert_eq!(
            without_static.relations.len() + 1,
            baseline.document().relations.len()
        );
        let prior =
            rootlight_ir::IrDocumentUsage::measure(&without_static, &IrLimits::default()).unwrap();
        assert_eq!(complete.total_records(), prior.total_records() + 1);
        assert_eq!(
            complete.total_nested_items(),
            prior.total_nested_items() + 3
        );
        let emitted = if record_bound {
            complete.total_records()
        } else {
            complete.total_nested_items()
        };
        let resource = if record_bound {
            ResourceKind::Records
        } else {
            ResourceKind::NestedItems
        };
        let execute_at = |maximum| {
            let mut ir = IrLimits::default();
            if record_bound {
                ir.max_total_records = maximum;
            } else {
                ir.max_total_nested_items = maximum;
            }
            let selected = limits(ir);
            let request = AnalysisRequest::new(
                GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
                LanguageId::new("apex").unwrap(),
                AnalysisTier::TierD,
                BuildContextIdentity::new(content_hash(b"capacity-build")),
                &selected,
            )
            .unwrap()
            .with_generated_status(false);
            execute_analysis(
                &prepared,
                &request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancellation,
            )
        };
        // The public transaction also meters flags, candidate targets and SDK
        // metadata, so an internal lowering reservation is not its full census.
        let mut maximum = 0;
        let mut admitted = None;
        let mut refusals = 0;
        for _ in 0..32 {
            match execute_at(maximum) {
                Ok(output) => {
                    admitted = Some(output);
                    break;
                }
                Err(AdapterError::Sink(SinkError::StreamLimit {
                    resource: actual,
                    observed,
                    limit,
                })) => {
                    assert_eq!(actual, resource);
                    assert_eq!(limit, maximum);
                    assert!(
                        observed > maximum,
                        "nonadvancing admission {observed}/{maximum}"
                    );
                    maximum = observed;
                    refusals += 1;
                }
                other => panic!("unexpected public admission at {maximum}: {other:?}"),
            }
        }
        let output =
            admitted.expect("typed public admission must reach a finite accepted boundary");
        assert!(refusals > 0);
        assert!(
            maximum >= emitted && maximum > 1,
            "public admitted={maximum}, emitted={emitted}, typed refusals={refusals}"
        );
        checked_virtual_capacity_output(&output, source, &source_ref);
        assert_eq!(output.document(), baseline.document());
        assert_eq!(
            execute_at(maximum - 1).unwrap_err(),
            AdapterError::Sink(SinkError::StreamLimit {
                resource,
                observed: maximum,
                limit: maximum - 1,
            })
        );
        let retry = execute_at(maximum).unwrap();
        checked_virtual_capacity_output(&retry, source, &source_ref);
        assert_eq!(retry.document(), baseline.document());
    }

    #[test]
    fn virtual_apex_total_nested_items_boundary_keeps_static_and_runtime_proofs() {
        virtual_apex_aggregate_capacity_boundary(false);
    }

    #[test]
    fn virtual_apex_total_records_boundary_keeps_static_and_runtime_proofs() {
        virtual_apex_aggregate_capacity_boundary(true);
    }
}
