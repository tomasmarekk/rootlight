//! Join native-proven written C calls to their same-file function entities.
//! The producer census proves lexical binding; compilation, external macros
//! and runtime behavior remain outside this source-level relation contract.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

pub(super) struct CallTargets<'a> {
    names: HashMap<&'a str, Option<SymbolId>>,
}

impl<'a> CallTargets<'a> {
    pub(super) fn new(
        entities: &'a HashMap<u64, MaterializedEntity>,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut names = HashMap::new();
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in entities.values() {
            cancellation.check()?;
            if entity.record.language != "c" || entity.record.kind != EntityKind::Function {
                continue;
            }
            if !matches!(entity.direct_parent, ContainerRef::File(_)) {
                continue;
            }
            names
                .entry(entity.record.canonical_name.as_str())
                .and_modify(|existing| {
                    if *existing != Some(entity.record.id) {
                        *existing = None;
                    }
                })
                .or_insert(Some(entity.record.id));
        }
        Ok(Self { names })
    }

    pub(super) fn resolve(
        &self,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        Ok(self.names.get(name).copied().flatten())
    }
}

/// Emit only the resolved native-proven call, with canonical proof before identity.
/// Failed allocation and cancellation propagate before the caller publishes it.
pub(super) fn call_relation(
    occurrence: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    symbol: SymbolId,
    cancellation: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancellation.check()?;
    let Some(owner) = owner
        .filter(|owner| owner.record.language == "c" && owner.record.kind == EntityKind::Function)
    else {
        return Ok(None);
    };
    if occurrence.syntax_kind != "c.direct_file_call.call"
        || occurrence.role != OccurrenceRole::CallSite
        || occurrence.enclosing != Some(owner.record.id)
        || occurrence.target != (OccurrenceTarget::Resolved { symbol })
    {
        return Ok(None);
    }
    let mut relation = lexical_reference_relation(occurrence, symbol)?;
    relation.subject = RelationEndpoint::Entity(owner.record.id);
    relation.predicate = RelationPredicate::Calls;
    relation
        .evidence
        .derivation
        .try_reserve_exact(3)
        .map_err(|_| SinkError::AllocationFailed)?;
    relation.evidence.derivation.extend([
        FactRef::Entity(owner.record.id),
        FactRef::Fact(occurrence.id),
        FactRef::Entity(symbol),
    ]);
    relation.evidence.derivation.sort_unstable();
    relation.evidence.derivation.dedup();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("c-direct-call-identity"))?;
    cancellation.check()?;
    Ok(Some(relation))
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
    fn native_written_call_preflight_reserves_relation_and_three_proof_parents() {
        let source = "int target(void){return 1;} int caller(void){return target();}";
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("module.c"), source).unwrap();
        let repository = derive_repository(b"c-module-capacity").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("module.c")).unwrap(),
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
            ProducerIdentity::new("c-module-capacity", "1.0", content_hash(b"test")).unwrap();
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(provider),
            producer.clone(),
            LanguageId::new("c").unwrap(),
            "native-module-capacity",
            content_hash(b"capacity-binary"),
        )
        .unwrap();
        let parse_limits = limits(IrLimits::default());
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
            LanguageId::new("c").unwrap(),
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
                .filter(|fact| fact.syntax_kind().as_str() == "c.direct_file_call.call")
                .count(),
            1
        );
        let entity_count = parsed
            .facts()
            .iter()
            .filter(|fact| structural_entity_kind(fact).is_some())
            .count();
        // Native C also retains unproved written references beside the call.
        // Admission reserves their possible edges before binding is decided.
        let reference_count = parsed
            .facts()
            .iter()
            .filter(|fact| {
                matches!(
                    occurrence_role(fact),
                    Some(OccurrenceRole::Reference | OccurrenceRole::CallSite)
                )
            })
            .count();
        let relation_count = entity_count + reference_count;
        assert!(reference_count >= 1);
        let mut exact_ir = IrLimits::default();
        exact_ir.max_relations = relation_count;
        let exact_limits = limits(exact_ir);
        let exact_request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
            LanguageId::new("c").unwrap(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"capacity-build")),
            &exact_limits,
        )
        .unwrap()
        .with_generated_status(false);
        preflight_lowering_limits(
            &exact_request,
            &parsed,
            &producer,
            "native-module-capacity",
            &[],
            &cancellation,
        )
        .unwrap();
        let output = execute_analysis(
            &PreparsedTreeSitterAnalyzer {
                analyzer: &analyzer,
                parse_output: &parsed,
            },
            &exact_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
        assert_eq!(
            output
                .document()
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Calls)
                .count(),
            1
        );
        for per_record in [true, false] {
            let mut ir = IrLimits::default();
            if per_record {
                ir.max_nested_items_per_record = 2;
            } else {
                ir.max_relations = relation_count - 1;
            }
            let budget = limits(ir);
            let request = AnalysisRequest::new(
                GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
                LanguageId::new("c").unwrap(),
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
                    matches!(rejected, Err(AdapterError::Sink(SinkError::StreamLimit {resource: ResourceKind::Records, observed, limit})) if observed == relation_count && limit == relation_count - 1)
                );
            }
        }
    }
}
