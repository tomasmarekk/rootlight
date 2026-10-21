//! Coq source declarations are independent of kernel checking and elaboration.
//! Flat Module/Section sentences retain owner gaps instead of invented scopes.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::MaterializedEntity;

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (
            EntityKind::Constant,
            "coq.definition.declaration"
                | "coq.theorem.declaration"
                | "coq.fixpoint.declaration"
                | "coq.instance.declaration"
                | "coq.declared_instance.declaration"
        ) | (
            EntityKind::Function,
            "coq.parameterized_assertion.declaration" | "coq.written_callable.declaration"
        ) | (EntityKind::Parameter, "coq.header_parameter.declaration")
            | (EntityKind::Enum, "coq.inductive.declaration")
            | (EntityKind::Struct, "coq.record.declaration")
            | (EntityKind::Class, "coq.class.declaration")
            | (
                EntityKind::Field,
                "coq.record_field.declaration" | "coq.class_field.declaration"
            )
            | (EntityKind::Constructor, "coq.constructor.declaration")
            | (
                EntityKind::Constructor,
                "coq.record_constructor.declaration"
            )
            | (
                EntityKind::Module,
                "coq.module_header.declaration" | "coq.module_type_header.declaration"
            )
    )
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "coq.file.module") => Some((
            FactDomain::Relations,
            "coq-elaboration-typechecking-proofchecking-and-term-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.module_ownership.signature") => Some((
            FactDomain::Relations,
            "coq-module-section-stack-and-qualified-ownership-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.instance_semantics.signature") => Some((
            FactDomain::Relations,
            "coq-instance-typeclass-registration-and-field-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.record_semantics.signature") => Some((
            FactDomain::Relations,
            "coq-record-class-elaboration-projection-generation-and-field-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.context_binding.signature") => Some((
            FactDomain::Entities,
            "coq-context-and-assumption-binder-lifetime-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.unmodeled_binder.signature") => Some((
            FactDomain::Entities,
            "coq-term-and-module-binder-entities-and-lifetime-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.assertion_header_unavailable.signature") => Some((
            FactDomain::Entities,
            "coq-written-assertion-header-ownership-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.header_formal_semantics.signature") => Some((
            FactDomain::Relations,
            "coq-written-formal-types-and-term-lifetime-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.notation_binding_unavailable.signature") => Some((
            FactDomain::Relations,
            "coq-declaration-time-notation-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.invalid_name.signature") => {
            Some((FactDomain::Entities, "coq-declaration-name-unavailable"))
        }
        (SyntaxFactKind::Signature, "coq.shared_definition.signature") => Some((
            FactDomain::Entities,
            "coq-mutual-definition-body-ownership-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.parser_recovery.signature") => Some((
            FactDomain::Entities,
            "coq-parser-recovery-structure-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.custom_syntax.signature") => Some((
            FactDomain::Entities,
            "coq-custom-notation-or-generic-tactic-structure-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.unmodeled_sentence.signature") => Some((
            FactDomain::Entities,
            "coq-vernacular-construct-structure-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.conditional_sentence.signature") => Some((
            FactDomain::Entities,
            "coq-fail-attribute-or-selector-declaration-effect-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.proof_checking.signature") => Some((
            FactDomain::Relations,
            "coq-proof-state-and-tactic-semantics-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "coq.written_reference.reference")
        .then_some("coq-name-resolution-and-implicit-argument-elaboration-unavailable")
}

pub(super) fn structural_gap(fact: &SyntaxFact) -> Option<&'static str> {
    source_gap(fact)
        .filter(|(domain, _)| *domain == FactDomain::Entities)
        .map(|(_, detail)| detail)
}

pub(super) fn term_surface_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "coq.file.module")
        .then_some("coq-term-pattern-tactic-and-notation-reference-surface-incomplete")
}

pub(super) fn entity_surface_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "coq.file.module")
        .then_some("coq-local-term-binder-projection-and-generated-declaration-coverage-incomplete")
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "coq.written_reference.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, "coq.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "coq.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn declaration_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    // These name anchors describe requirements or written fields. They do not
    // prove an instance implementation or a generated global projection.
    (fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "coq.declared_instance_name.definition"
                | "coq.record_field_name.definition"
                | "coq.class_field_name.definition"
        ))
    .then_some(OccurrenceRole::Declaration)
}
struct NotationBinding {
    symbol: SymbolId,
    reference: SourceSpan,
    spelling: rootlight_ids::ContentHash,
    provenance: rootlight_ids::FactId,
}

pub(super) fn notation_ends(
    facts: &[SyntaxFact],
    cancellation: &Cancellation,
) -> Result<HashMap<(rootlight_ids::FileId, u64), ()>, AdapterError> {
    let mut ends = HashMap::new();
    for fact in facts {
        cancellation.check()?;
        if fact.kind() == SyntaxFactKind::Signature
            && matches!(
                fact.syntax_kind().as_str(),
                "coq.notation_target.signature" | "coq.written_target.signature"
            )
        {
            ends.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            ends.insert((fact.span().file(), fact.span().end_byte()), ());
        }
    }
    Ok(ends)
}

pub(super) struct NotationTargets {
    bindings: HashMap<u64, NotationBinding>,
    source: SourceRef,
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|value| *value = None)
        .or_insert(Some(value));
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}

impl NotationTargets {
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        full_source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut declarations = HashMap::new();
        let mut definitions = HashMap::new();
        let mut references = HashMap::new();
        let mut markers = HashMap::new();
        for map in [
            &mut declarations,
            &mut definitions,
            &mut references,
            &mut markers,
        ] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        for fact in facts {
            cancellation.check()?;
            if fact.span().file() != full_source.span().file()
                || fact.span().start_byte() < full_source.span().start_byte()
                || fact.span().end_byte() > full_source.span().end_byte()
            {
                continue;
            }
            if fact.kind() == SyntaxFactKind::Declaration {
                unique(&mut declarations, fact.span().start_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "coq.name.definition"
                && let Some(parent) = fact.parent()
            {
                unique(&mut definitions, parent, fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "coq.written_reference.reference"
            {
                unique(&mut references, fact.span().end_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "coq.notation_target.signature"
            {
                unique(&mut markers, fact.span().end_byte(), fact);
            }
        }
        let mut bindings = HashMap::new();
        bindings
            .try_reserve(markers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for marker in markers.values().copied().flatten() {
            cancellation.check()?;
            let Some(declaration) = declarations
                .get(&marker.span().start_byte())
                .copied()
                .flatten()
            else {
                continue;
            };
            let expected = match declaration.syntax_kind().as_str() {
                "coq.record.declaration" => EntityKind::Struct,
                "coq.class.declaration" => EntityKind::Class,
                "coq.definition.declaration" => EntityKind::Constant,
                "coq.written_callable.declaration" => EntityKind::Function,
                _ => continue,
            };
            let Some(definition) = definitions.get(&declaration.local_id()).copied().flatten()
            else {
                continue;
            };
            let Some(reference) = references.get(&marker.span().end_byte()).copied().flatten()
            else {
                continue;
            };
            let Some(entity) = entities.get(&declaration.local_id()) else {
                continue;
            };
            let Some(source) = entity.record.evidence.source.as_ref() else {
                continue;
            };
            if entity.record.language != "coq"
                || entity.record.kind != expected
                || entity.record.repository != full_source.repository()
                || entity.record.generation != full_source.generation()
                || entity.definition_local_id != Some(definition.local_id())
                || !same_source(source, full_source)
                || source.span() != declaration.span()
                || definition.span().start_byte() < declaration.span().start_byte()
                || definition.span().end_byte() > declaration.span().end_byte()
                || reference.span().start_byte() <= declaration.span().end_byte()
                || text(definition.span())? != entity.record.canonical_name
                || text(reference.span())? != entity.record.canonical_name
            {
                continue;
            }
            bindings.insert(
                reference.local_id(),
                NotationBinding {
                    symbol: entity.record.id,
                    reference: reference.span(),
                    spelling: rootlight_ids::content_hash(text(reference.span())?.as_bytes()),
                    provenance: entity.record.provenance,
                },
            );
        }
        cancellation.check()?;
        Ok(Self {
            bindings,
            source: full_source.clone(),
        })
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        site: &OccurrenceRecord,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if site.role != OccurrenceRole::Reference
            || site.syntax_kind != "coq.written_reference.reference"
            || fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "coq.written_reference.reference"
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
                binding.reference == fact.span()
                    && binding.reference == site.source.span()
                    && binding.spelling == site.syntactic_text_hash
                    && binding.provenance == site.provenance
            })
            .map(|binding| binding.symbol))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId, content_hash};
    use rootlight_ir::{
        AnalysisTier, ContainerRef, EntityRecord, EntityVisibility, FactEvidence,
        SymbolIdentityClaim,
    };

    const TEXT: &str = "Definition value := 0. Notation \"! T\" := (value T).";

    fn span(start: usize, end: usize) -> SourceSpan {
        SourceSpan::new(
            FileId::from_bytes([2; 20]),
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        )
        .unwrap()
    }

    fn source() -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([3; 20]),
            span(0, TEXT.len()),
            content_hash(TEXT.as_bytes()),
            None,
        )
    }

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        range: SourceSpan,
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            range,
            usize::try_from(id).unwrap(),
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn witnesses() -> Vec<SyntaxFact> {
        let end = TEXT.rfind("value").unwrap() + 5;
        vec![
            fact(
                1,
                None,
                SyntaxFactKind::Declaration,
                span(0, TEXT.find('.').unwrap()),
                "coq.definition.declaration",
            ),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Occurrence,
                span(11, 16),
                "coq.name.definition",
            ),
            fact(
                3,
                None,
                SyntaxFactKind::Occurrence,
                span(end - 5, end),
                "coq.written_reference.reference",
            ),
            fact(
                4,
                None,
                SyntaxFactKind::Signature,
                span(0, end),
                "coq.notation_target.signature",
            ),
        ]
    }

    fn entities() -> HashMap<u64, MaterializedEntity> {
        let source = source();
        let owner = ContainerRef::File(source.span().file());
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: source.repository(),
            language: "coq".to_owned(),
            kind: EntityKind::Constant,
            container: Some(owner),
            container_identity: source.span().file().as_bytes().to_vec(),
            declared_identity: "value".to_owned(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: content_hash(b"coq-notation-test").as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        let record = EntityRecord {
            id: claim.symbol,
            repository: source.repository(),
            generation: source.generation(),
            kind: EntityKind::Constant,
            language: "coq".to_owned(),
            tier: AnalysisTier::TierD,
            canonical_name: "value".to_owned(),
            display_name: "value".to_owned(),
            qualified_name: "value".to_owned(),
            container: Some(owner),
            visibility: EntityVisibility::Unknown,
            flags: Vec::new(),
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: Some(SourceRef::new(
                    source.repository(),
                    source.generation(),
                    witnesses()[0].span(),
                    source.content_hash(),
                    None,
                )),
                derivation: Vec::new(),
            },
        };
        HashMap::from([(
            1,
            MaterializedEntity {
                record,
                identity_claim: claim,
                direct_parent: owner,
                identity_guard: [0; 32],
                definition_local_id: Some(2),
                signature_evidence: None,
                signature_span: None,
            },
        )])
    }

    fn plan(facts: &[SyntaxFact], entities: &HashMap<u64, MaterializedEntity>) -> NotationTargets {
        NotationTargets::new(
            facts,
            entities,
            &source(),
            |span| {
                Ok(&TEXT[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()])
            },
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn exact_marker_declaration_name_and_reference_are_jointly_mandatory() {
        let original = witnesses();
        assert_eq!(plan(&original, &entities()).bindings.len(), 1);
        for missing in 0..4 {
            let mut facts = original.clone();
            facts.remove(missing);
            assert!(
                plan(&facts, &entities()).bindings.is_empty(),
                "missing={missing}"
            );
        }
        for duplicate in 0..4 {
            let mut facts = original.clone();
            let witness = &original[duplicate];
            facts.push(fact(
                5,
                witness.parent(),
                witness.kind(),
                witness.span(),
                witness.syntax_kind().as_str(),
            ));
            assert!(
                plan(&facts, &entities()).bindings.is_empty(),
                "duplicate={duplicate}"
            );
        }
        for range in [
            span(11, usize::try_from(original[3].span().end_byte()).unwrap()),
            span(0, TEXT.len()),
        ] {
            let mut facts = original.clone();
            facts[3] = fact(
                4,
                None,
                SyntaxFactKind::Signature,
                range,
                "coq.notation_target.signature",
            );
            assert!(plan(&facts, &entities()).bindings.is_empty());
        }
        let mut facts = original;
        facts[1] = fact(
            2,
            Some(99),
            SyntaxFactKind::Occurrence,
            span(11, 16),
            "coq.name.definition",
        );
        assert!(plan(&facts, &entities()).bindings.is_empty());
    }

    #[test]
    fn adopted_identity_requires_current_source_kind_range_and_exact_definition_id() {
        for changed in 0..8 {
            let mut entities = entities();
            let entity = entities.get_mut(&1).unwrap();
            let previous = entity.record.evidence.source.as_ref().unwrap();
            let (mut repository, mut generation, mut hash, mut range) = (
                previous.repository(),
                previous.generation(),
                previous.content_hash(),
                previous.span(),
            );
            match changed {
                0 => repository = RepositoryId::from_bytes([9; 16]),
                1 => generation = GenerationId::from_bytes([9; 20]),
                2 => hash = content_hash(b"another-content"),
                3 => {
                    range = SourceSpan::new(
                        FileId::from_bytes([9; 20]),
                        range.start_byte(),
                        range.end_byte(),
                    )
                    .unwrap()
                }
                4 => range = span(11, 16),
                5 => entity.record.kind = EntityKind::Function,
                6 => entity.record.canonical_name = "another".to_owned(),
                _ => entity.definition_local_id = Some(99),
            }
            entity.record.evidence.source =
                Some(SourceRef::new(repository, generation, range, hash, None));
            assert!(
                plan(&witnesses(), &entities).bindings.is_empty(),
                "change={changed}"
            );
        }
    }

    #[test]
    fn notation_planning_observes_cancellation_before_allocating_or_adopting_targets() {
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            NotationTargets::new(&[], &HashMap::new(), &source(), |_| Ok(""), &cancel),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn reference_adoption_rechecks_current_occurrence_source_role_hash_and_provenance() {
        let facts = witnesses();
        let targets = plan(&facts, &entities());
        let full = source();
        let reference = SourceRef::new(
            full.repository(),
            full.generation(),
            facts[2].span(),
            full.content_hash(),
            None,
        );
        let original = OccurrenceRecord {
            id: FactId::from_bytes([5; 20]),
            repository: full.repository(),
            generation: full.generation(),
            file: full.span().file(),
            source: reference.clone(),
            role: OccurrenceRole::Reference,
            enclosing: None,
            target: OccurrenceTarget::Unresolved {
                text_hash: content_hash(b"value"),
            },
            syntactic_text_hash: content_hash(b"value"),
            syntax_kind: "coq.written_reference.reference".to_owned(),
            provenance: FactId::from_bytes([4; 20]),
            confidence: rootlight_ir::Confidence::new(500).unwrap(),
            evidence: FactEvidence {
                source: Some(reference),
                derivation: Vec::new(),
            },
        };
        assert!(
            targets
                .resolve(&facts[2], &original, &Cancellation::new())
                .unwrap()
                .is_some()
        );
        for changed in 0..9 {
            let mut site = original.clone();
            match changed {
                0 => site.repository = RepositoryId::from_bytes([9; 16]),
                1 => site.generation = GenerationId::from_bytes([9; 20]),
                2 => site.file = FileId::from_bytes([9; 20]),
                3 => {
                    site.source = SourceRef::new(
                        full.repository(),
                        full.generation(),
                        site.source.span(),
                        content_hash(b"foreign"),
                        None,
                    )
                }
                4 => site.role = OccurrenceRole::Definition,
                5 => site.syntax_kind = "other.reference".to_owned(),
                6 => site.syntactic_text_hash = content_hash(b"foreign"),
                7 => site.provenance = FactId::from_bytes([9; 20]),
                _ => {
                    site.target = OccurrenceTarget::Resolved {
                        symbol: SymbolId::from_bytes([9; 20]),
                    }
                }
            }
            assert_eq!(
                targets
                    .resolve(&facts[2], &site, &Cancellation::new())
                    .unwrap(),
                None,
                "change={changed}"
            );
        }
    }

    #[test]
    fn scope_markers_and_disjoint_semantic_parents_are_not_lexical_certificates() {
        let mut facts = witnesses();
        assert!(!crate::lowering::is_signature_capture(&facts[3]));
        let formal_semantics = fact(
            5,
            None,
            SyntaxFactKind::Signature,
            span(11, 16),
            "coq.header_formal_semantics.signature",
        );
        assert!(!crate::lowering::is_signature_capture(&formal_semantics));
        facts[3] = fact(
            4,
            None,
            SyntaxFactKind::Scope,
            facts[3].span(),
            "coq.notation_target.signature",
        );
        assert!(plan(&facts, &entities()).bindings.is_empty());
        facts = witnesses();
        facts[2] = fact(
            3,
            Some(1),
            SyntaxFactKind::Occurrence,
            facts[2].span(),
            "coq.written_reference.reference",
        );
        assert!(
            matches!(crate::lowering::validate_fact_graph(&facts, &[], &source(), &Cancellation::new()),
            Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-lowering-parent")
        );
    }

    #[test]
    fn partial_native_prefix_reserves_notation_edges_in_relation_and_total_record_bounds() {
        use crate::lowering::preflight_lowering_limits;
        use rootlight_adapter_sdk::{
            AnalysisLimits, AnalysisRequest, BatchThresholds, GenerationBoundSnapshot, LanguageId,
            ResourceKind, StreamLimits, structural_entity_kind,
        };
        use rootlight_ir::{BuildContextIdentity, CoverageStatus, IrLimits, ProducerIdentity};
        use rootlight_vfs::{RelativePath, RepositoryRoot};
        use std::{path::Path, sync::Arc};

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
        let original =
            include_bytes!("../../../rootlight-service/tests/fixtures/coq-squash/MRSquash.v");
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("written.v"), original).unwrap();
        let repository = RepositoryId::from_bytes([1; 16]);
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("written.v")).unwrap(),
                1024 * 1024,
            )
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([3; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(original.len()).unwrap()).unwrap(),
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
            ProducerIdentity::new("coq-notation-capacity", "1.0", content_hash(b"coq-test"))
                .unwrap();
        let analyzer = crate::TreeSitterAnalyzer::new(
            Arc::new(provider),
            producer.clone(),
            LanguageId::new("coq").unwrap(),
            "native-coq-capacity",
            content_hash(b"binary"),
        )
        .unwrap();
        let parse_limits = limits(IrLimits::default());
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("coq").unwrap(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"context")),
            &parse_limits,
        )
        .unwrap()
        .with_generated_status(false);
        let cancel = Cancellation::new();
        let parsed = analyzer.parse_request(&request, &cancel).unwrap();
        assert_ne!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "coq.notation_target.signature")
                .count(),
            1
        );
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(
                    |fact| fact.syntax_kind().as_str() == "coq.written_reference.reference"
                        && fact.span().start_byte() == 79
                        && fact.span().end_byte() == 85
                )
                .count(),
            1
        );
        let upper_relations = parsed
            .facts()
            .iter()
            .filter(|fact| structural_entity_kind(fact).is_some())
            .count()
            + 1;
        let upper = preflight_lowering_limits(
            &request,
            &parsed,
            &producer,
            "native-coq-capacity",
            &[],
            &cancel,
        )
        .unwrap();
        for total in [false, true] {
            let mut ir = IrLimits::default();
            let expected = if total {
                ir.max_total_records = upper.records - 1;
                upper.records
            } else {
                ir.max_relations = upper_relations - 1;
                upper_relations
            };
            let bounded = limits(ir);
            let request = AnalysisRequest::new(
                GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
                LanguageId::new("coq").unwrap(),
                AnalysisTier::TierD,
                BuildContextIdentity::new(content_hash(b"context")),
                &bounded,
            )
            .unwrap()
            .with_generated_status(false);
            assert!(
                matches!(preflight_lowering_limits(&request, &parsed, &producer, "native-coq-capacity", &[], &cancel),
                Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::Records, observed, limit }))
                    if observed == expected && limit == expected - 1)
            );
        }
    }
}
