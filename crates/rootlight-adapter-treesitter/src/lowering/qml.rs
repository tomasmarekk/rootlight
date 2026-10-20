//! Join only native-certified QML id calls to their exact same-file methods.
//! File-wide unique method names are an additional conservative join condition.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    EntityKind, FactDomain, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Variable, "qml.id_variable.declaration")
            | (EntityKind::Property, "qml.property.declaration")
            | (EntityKind::Method, "qml.method.declaration")
    )
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "qml.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "qml.unique_id_method.call" | "qml.unresolved.call") => {
            Some(OccurrenceRole::CallSite)
        }
        (SyntaxFactKind::Comment, "qml.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "qml.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "qml.file.module") => &[
            (
                FactDomain::Entities,
                "qml-signals-enums-components-unicode-and-general-javascript-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "qml-properties-parameters-locals-aliases-and-general-name-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "qml-imported-types-implicit-components-dynamic-qt-runtime-and-general-binding-unavailable",
            ),
        ],
        (SyntaxFactKind::Declaration, "qml.unsupported.declaration") => &[(
            FactDomain::Entities,
            "qml-written-declaration-kind-unavailable",
        )],
        (SyntaxFactKind::Declaration, "qml.unavailable_id.declaration") => &[
            (
                FactDomain::Entities,
                "qml-written-object-id-identity-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "qml-written-object-id-definition-unavailable",
            ),
            (
                FactDomain::Relations,
                "qml-written-object-id-ownership-unavailable",
            ),
        ],
        (SyntaxFactKind::Signature, "qml.unsupported.expression") => &[(
            FactDomain::Relations,
            "qml-written-construct-semantics-unavailable",
        )],
        _ => &[],
    };
    rows.iter().copied()
}

pub(super) struct CallTargets<'a> {
    names: HashMap<&'a str, Option<&'a MaterializedEntity>>,
}

impl<'a> CallTargets<'a> {
    pub(super) fn new(
        entities: &'a HashMap<u64, MaterializedEntity>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut names = HashMap::new();
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in entities.values() {
            cancel.check()?;
            if entity.record.language != "qml" || entity.record.kind != EntityKind::Method {
                continue;
            }
            names
                .entry(entity.record.canonical_name.as_str())
                .and_modify(|value| *value = None)
                .or_insert(Some(entity));
        }
        Ok(Self { names })
    }

    pub(super) fn resolve(
        &self,
        name: &str,
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if site.role != OccurrenceRole::CallSite || site.syntax_kind != "qml.unique_id_method.call"
        {
            return Ok(None);
        }
        let Some(target) = self.names.get(name).copied().flatten() else {
            return Ok(None);
        };
        let Some(source) = target.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        Ok((source.repository() == site.source.repository()
            && source.generation() == site.source.generation()
            && source.content_hash() == site.source.content_hash()
            && source.span().file() == site.source.span().file())
        .then_some(target.record.id))
    }
}

pub(super) fn call_relation(
    site: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    target: SymbolId,
    cancel: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancel.check()?;
    let Some(owner) = owner
        .filter(|owner| owner.record.language == "qml" && owner.record.kind == EntityKind::Method)
    else {
        return Ok(None);
    };
    if site.syntax_kind != "qml.unique_id_method.call"
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
        .map_err(|_| provider_failure("qml-written-call-identity"))?;
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
    fn qml_native_call_preflight_reserves_relation_and_three_proof_parents() {
        let source = "import QtQuick
Item { id: root; function target() {} function caller() { root.target(); } }";
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("module.qml"), source).unwrap();
        let repository = derive_repository(b"qml-module-capacity").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("module.qml")).unwrap(),
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
            ProducerIdentity::new("qml-module-capacity", "1.0", content_hash(b"test")).unwrap();
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(provider),
            producer.clone(),
            LanguageId::new("qml").unwrap(),
            "native-module-capacity",
            content_hash(b"capacity-binary"),
        )
        .unwrap();
        let parse_limits = limits(IrLimits::default());
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
            LanguageId::new("qml").unwrap(),
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
                .filter(|fact| fact.syntax_kind().as_str() == "qml.unique_id_method.call")
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
                LanguageId::new("qml").unwrap(),
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
