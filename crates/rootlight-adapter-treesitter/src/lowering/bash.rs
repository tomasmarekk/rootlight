//! Join native-proven module reads to exact same-frame source definitions.
//! Only a containing module assignment gains a dependency; function dynamic
//! scope and runtime shell evaluation are deliberately not inferred here.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationRecord,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

pub(super) struct Bindings<'a> {
    names: HashMap<&'a str, Option<&'a MaterializedEntity>>,
    modules: HashSet<SymbolId>,
}

impl<'a> Bindings<'a> {
    pub(super) fn new(
        entities: &'a HashMap<u64, MaterializedEntity>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut modules = HashSet::new();
        modules
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in entities.values() {
            cancel.check()?;
            if entity.record.language == "bash" && entity.record.kind == EntityKind::Module {
                modules.insert(entity.record.id);
            }
        }
        let mut names = HashMap::new();
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in entities.values() {
            cancel.check()?;
            if entity.record.language != "bash"
                || entity.record.kind != EntityKind::Variable
                || !matches!(entity.direct_parent, ContainerRef::Entity(owner) if modules.contains(&owner))
            {
                continue;
            }
            names
                .entry(entity.record.canonical_name.as_str())
                .and_modify(|old| *old = None)
                .or_insert(Some(entity));
        }
        Ok(Self { names, modules })
    }

    pub(super) fn resolve(
        &self,
        name: &str,
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(target) = self.names.get(name).copied().flatten() else {
            return Ok(None);
        };
        let Some(source) = target.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        Ok((source.repository() == site.source.repository()
            && source.generation() == site.source.generation()
            && source.content_hash() == site.source.content_hash()
            && source.span().file() == site.source.span().file()
            && source.span().end_byte() <= site.source.span().start_byte())
        .then_some(target.record.id))
    }

    /// Derive a dependency only for a source-owned variable value containing the read.
    /// Allocation/cancellation failures propagate before publication or identity creation.
    pub(super) fn dependency(
        &self,
        site: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        target: SymbolId,
        cancel: &Cancellation,
    ) -> Result<Option<RelationRecord>, AdapterError> {
        cancel.check()?;
        let Some(owner) = owner.filter(|owner| owner.record.language == "bash"
            && owner.record.kind == EntityKind::Variable
            && matches!(owner.direct_parent, ContainerRef::Entity(id) if self.modules.contains(&id)))
        else { return Ok(None); };
        if site.role != OccurrenceRole::Read
            || site.syntax_kind != "bash.module_read.reference"
            || site.enclosing != Some(owner.record.id)
            || site.target != (OccurrenceTarget::Resolved { symbol: target })
        {
            return Ok(None);
        }
        let Some(source) = owner.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if source.repository() != site.source.repository()
            || source.generation() != site.source.generation()
            || source.content_hash() != site.source.content_hash()
            || source.span().file() != site.source.span().file()
            || source.span().start_byte() > site.source.span().start_byte()
            || source.span().end_byte() < site.source.span().end_byte()
        {
            return Ok(None);
        }
        let mut relation = lexical_reference_relation(site, target)?;
        relation.subject = RelationEndpoint::Entity(owner.record.id);
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
        relation.id = derive_relation_record_id(&relation)
            .map_err(|_| provider_failure("bash-read-dependency-identity"))?;
        cancel.check()?;
        Ok(Some(relation))
    }
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
    fn module_read_dependency_preflight_reserves_relation_and_three_proof_parents() {
        let source = "REPO=outer\nprs=$REPO\n";
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("module.sh"), source).unwrap();
        let repository = derive_repository(b"bash-module-capacity").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("module.sh")).unwrap(),
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
            ProducerIdentity::new("bash-module-capacity", "1.0", content_hash(b"test")).unwrap();
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(provider),
            producer.clone(),
            LanguageId::new("bash").unwrap(),
            "native-module-capacity",
            content_hash(b"capacity-binary"),
        )
        .unwrap();
        let parse_limits = limits(IrLimits::default());
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
            LanguageId::new("bash").unwrap(),
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
                .filter(|fact| fact.syntax_kind().as_str() == "bash.module_read.reference")
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
                LanguageId::new("bash").unwrap(),
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
}
