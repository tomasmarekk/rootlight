//! Join native-certified direct Fortran calls within their exact written module.
//! ASCII folding affects lookup only; authored identity, source coordinates and
//! the generic resolver's unresolved-Fortran fence remain unchanged.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

fn folded(name: &str) -> Result<String, AdapterError> {
    let mut result = String::new();
    result
        .try_reserve_exact(name.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    result.push_str(name);
    result.make_ascii_lowercase();
    Ok(result)
}

fn module(entity: &MaterializedEntity) -> Option<SymbolId> {
    match (entity.record.container, entity.direct_parent) {
        (Some(ContainerRef::Entity(written)), ContainerRef::Entity(direct))
            if written == direct =>
        {
            Some(written)
        }
        _ => None,
    }
}

fn same_frame(entity: &MaterializedEntity, site: &OccurrenceRecord) -> bool {
    entity
        .record
        .evidence
        .source
        .as_ref()
        .is_some_and(|source| {
            source.repository() == site.source.repository()
                && source.generation() == site.source.generation()
                && source.content_hash() == site.source.content_hash()
                && source.span().file() == site.source.span().file()
        })
}

pub(super) struct CallTargets<'a> {
    modules: HashMap<SymbolId, &'a MaterializedEntity>,
    owners: HashSet<SymbolId>,
    names: HashMap<(SymbolId, String), Option<&'a MaterializedEntity>>,
}

impl<'a> CallTargets<'a> {
    pub(super) fn new(
        entities: &'a HashMap<u64, MaterializedEntity>,
        facts: &[SyntaxFact],
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            modules: HashMap::new(),
            owners: HashSet::new(),
            names: HashMap::new(),
        };
        result
            .modules
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .owners
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut subroutines = HashSet::new();
        subroutines
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            let Some(entity) = entities
                .get(&fact.local_id())
                .filter(|entity| entity.record.language == "fortran")
            else {
                continue;
            };
            match fact.syntax_kind().as_str() {
                "fortran.module.module" => {
                    result.modules.insert(entity.record.id, entity);
                }
                "fortran.function.declaration" | "fortran.subroutine.declaration"
                    if entity.record.kind == EntityKind::Function =>
                {
                    result.owners.insert(entity.record.id);
                    if fact.syntax_kind().as_str() == "fortran.subroutine.declaration" {
                        subroutines.insert(entity.record.id);
                    }
                }
                _ => {}
            }
        }
        for entity in entities
            .values()
            .filter(|entity| entity.record.language == "fortran")
        {
            cancel.check()?;
            let Some(parent) = module(entity).filter(|parent| result.modules.contains_key(parent))
            else {
                continue;
            };
            let key = (parent, folded(&entity.record.canonical_name)?);
            let target = subroutines.contains(&entity.record.id).then_some(entity);
            result
                .names
                .entry(key)
                .and_modify(|existing| *existing = None)
                .or_insert(target);
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        name: &str,
        site: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if site.syntax_kind != "fortran.direct_module_subroutine.call"
            || site.role != OccurrenceRole::CallSite
        {
            return Ok(None);
        }
        let Some(owner) = owner.filter(|owner| {
            self.owners.contains(&owner.record.id)
                && site.enclosing == Some(owner.record.id)
                && same_frame(owner, site)
        }) else {
            return Ok(None);
        };
        let Some(parent) = module(owner) else {
            return Ok(None);
        };
        let Some(written_module) = self
            .modules
            .get(&parent)
            .filter(|module| module.record.kind == EntityKind::Module && same_frame(module, site))
        else {
            return Ok(None);
        };
        let Some(target) = self
            .names
            .get(&(written_module.record.id, folded(name)?))
            .copied()
            .flatten()
            .filter(|target| {
                target.record.kind == EntityKind::Function
                    && module(target) == Some(parent)
                    && same_frame(target, site)
            })
        else {
            return Ok(None);
        };
        Ok(Some(target.record.id))
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
        owner.record.language == "fortran"
            && owner.record.kind == EntityKind::Function
            && same_frame(owner, site)
    }) else {
        return Ok(None);
    };
    if site.syntax_kind != "fortran.direct_module_subroutine.call"
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
        .map_err(|_| provider_failure("fortran-direct-call-identity"))?;
    cancel.check()?;
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
    fn fortran_native_call_preflight_reserves_relation_and_three_proof_parents() {
        let source = "module Sample\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nsubroutine Work()\nend subroutine Work\nend module Sample\n";
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("module.f90"), source).unwrap();
        let repository = derive_repository(b"fortran-module-capacity").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("module.f90")).unwrap(),
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
            ProducerIdentity::new("fortran-module-capacity", "1.0", content_hash(b"test")).unwrap();
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(provider),
            producer.clone(),
            LanguageId::new("fortran").unwrap(),
            "native-module-capacity",
            content_hash(b"capacity-binary"),
        )
        .unwrap();
        let parse_limits = limits(IrLimits::default());
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
            LanguageId::new("fortran").unwrap(),
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
                .filter(
                    |fact| fact.syntax_kind().as_str() == "fortran.direct_module_subroutine.call"
                )
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
                LanguageId::new("fortran").unwrap(),
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
