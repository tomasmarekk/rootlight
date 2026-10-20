//! Bind Ring formal Reads only with both endpoints of a native certificate.
//! The consumer rechecks the Parameter, Method, Class and current source frame.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    SourceRef, SourceSpan,
};

use super::MaterializedEntity;

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Class, "ring.class.declaration")
            | (EntityKind::Function, "ring.function.declaration")
            | (EntityKind::Method, "ring.method.declaration")
            | (EntityKind::Parameter, "ring.parameter.declaration")
    )
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "ring.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "ring.parameter_read.reference" | "ring.unresolved_read.reference",
        ) => Some(OccurrenceRole::Read),
        (SyntaxFactKind::Comment, "ring.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "ring.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "ring.file.module") => &[
            (
                FactDomain::Entities,
                "ring-multi-class-package-field-global-typed-and-general-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "ring-general-reads-writes-dynamic-and-type-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "ring-general-binding-dsl-keyword-remapping-inheritance-and-runtime-unavailable",
            ),
        ],
        (SyntaxFactKind::Declaration, "ring.unsupported.declaration") => &[
            (
                FactDomain::Entities,
                "ring-written-declaration-identity-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "ring-written-definition-unavailable",
            ),
            (FactDomain::Relations, "ring-written-owner-unavailable"),
        ],
        (SyntaxFactKind::Signature, "ring.construct_unavailable.expression") => &[
            (
                FactDomain::Entities,
                "ring-source-construct-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "ring-source-construct-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "ring-source-construct-semantics-unavailable",
            ),
        ],
        _ => &[],
    };
    rows.iter().copied()
}

struct Binding {
    target: SymbolId,
    method: SymbolId,
    span: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
}

pub(super) struct ReadTargets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|previous| *previous = None)
        .or_insert(Some(value));
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.span().file() == right.span().file()
        && left.content_hash() == right.content_hash()
}

impl ReadTargets {
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut declarations = HashMap::new();
        let mut definitions = HashMap::new();
        let mut reads = HashMap::new();
        let mut markers = HashMap::new();
        let mut methods = HashMap::new();
        for map in [
            &mut declarations,
            &mut definitions,
            &mut reads,
            &mut markers,
            &mut methods,
        ] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        let mut by_symbol = HashMap::new();
        by_symbol
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            cancel.check()?;
            unique(&mut by_symbol, entity.record.id, (*local, entity));
        }
        for fact in facts {
            cancel.check()?;
            if fact.span().file() != source.span().file() {
                continue;
            }
            let physical_span = physical(fact.span())?;
            if physical_span.start_byte() < source.span().start_byte()
                || physical_span.end_byte() > source.span().end_byte()
            {
                continue;
            }
            if fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == "ring.parameter.declaration"
            {
                unique(&mut declarations, physical_span.start_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == "ring.method.declaration"
            {
                unique(&mut methods, physical_span.start_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "ring.name.definition"
                && let Some(parent) = fact.parent()
            {
                unique(&mut definitions, parent, fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "ring.parameter_read.reference"
            {
                unique(&mut reads, physical_span.end_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "ring.parameter_read_target.signature"
            {
                unique(&mut markers, physical_span.end_byte(), fact);
            }
        }
        let mut bindings = HashMap::new();
        bindings
            .try_reserve(markers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for marker in markers.values().copied().flatten() {
            cancel.check()?;
            let marker_span = physical(marker.span())?;
            let Some(declaration) = declarations
                .get(&marker_span.start_byte())
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some(definition) = definitions.get(&declaration.local_id()).copied().flatten()
            else {
                continue;
            };
            let Some(read) = reads.get(&marker_span.end_byte()).copied().flatten() else {
                continue;
            };
            let declaration_span = physical(declaration.span())?;
            let definition_span = physical(definition.span())?;
            let read_span = physical(read.span())?;
            let Some(parameter) = entities.get(&declaration.local_id()) else {
                continue;
            };
            let Some(parameter_source) = parameter.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(ContainerRef::Entity(method_id)) = parameter.record.container else {
                continue;
            };
            let Some((method_local, method)) = by_symbol.get(&method_id).copied().flatten() else {
                continue;
            };
            let Some(method_source) = method.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(ContainerRef::Entity(class_id)) = method.record.container else {
                continue;
            };
            let Some((_, class)) = by_symbol.get(&class_id).copied().flatten() else {
                continue;
            };
            let Some(class_source) = class.record.evidence.source.as_ref() else {
                continue;
            };
            if class.record.language != "ring"
                || class.record.kind != EntityKind::Class
                || !same_source(class_source, source)
                || class.record.repository != source.repository()
                || class.record.generation != source.generation()
                || method_source.span().start_byte() < class_source.span().start_byte()
                || method_source.span().end_byte() > class_source.span().end_byte()
                || method.direct_parent != ContainerRef::Entity(class_id)
                || parameter.direct_parent != ContainerRef::Entity(method_id)
            {
                continue;
            }
            let Some(method_declaration) = methods
                .get(&method_source.span().start_byte())
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some(method_definition) = definitions.get(&method_local).copied().flatten() else {
                continue;
            };
            let method_declaration_span = physical(method_declaration.span())?;
            let method_definition_span = physical(method_definition.span())?;
            if parameter.record.language != "ring"
                || parameter.record.kind != EntityKind::Parameter
                || method.record.language != "ring"
                || method.record.kind != EntityKind::Method
                || parameter.record.repository != source.repository()
                || parameter.record.generation != source.generation()
                || method.record.repository != source.repository()
                || method.record.generation != source.generation()
                || parameter.definition_local_id != Some(definition.local_id())
                || method.definition_local_id != Some(method_definition.local_id())
                || !same_source(parameter_source, source)
                || !same_source(method_source, source)
                || parameter_source.span() != declaration_span
                || method_declaration.local_id() != method_local
                || method_declaration_span != method_source.span()
                || text(method_definition.span())? != method.record.canonical_name
                || definition_span.start_byte() < declaration_span.start_byte()
                || definition_span.end_byte() > declaration_span.end_byte()
                || method_definition_span.start_byte() < method_source.span().start_byte()
                || method_definition_span.end_byte() > method_source.span().end_byte()
                || declaration_span.start_byte() < method_source.span().start_byte()
                || read_span.end_byte() > method_source.span().end_byte()
                || read_span.start_byte() < declaration_span.end_byte()
                || text(definition.span())? != parameter.record.canonical_name
                || !text(read.span())?.eq_ignore_ascii_case(&parameter.record.canonical_name)
            {
                continue;
            }
            bindings.insert(
                read.local_id(),
                Binding {
                    target: parameter.record.id,
                    method: method_id,
                    span: read_span,
                    spelling: content_hash(text(read.span())?.as_bytes()),
                    provenance: parameter.record.provenance,
                },
            );
        }
        cancel.check()?;
        Ok(Self {
            source: source.clone(),
            bindings,
        })
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical_span: SourceSpan,
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<(SymbolId, SymbolId)>, AdapterError> {
        cancel.check()?;
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "ring.parameter_read.reference"
            || site.syntax_kind != "ring.parameter_read.reference"
            || site.role != OccurrenceRole::Read
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
                    && binding.provenance == site.provenance
            })
            .map(|binding| (binding.target, binding.method)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lowering::unresolved_occurrence;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{FileId, GenerationId, RepositoryId};
    use rootlight_ir::{
        AnalysisTier, EntityRecord, EntityVisibility, FactEvidence, SymbolIdentityClaim,
    };

    const TEXT: &str = "func measure value { consume(value) }";

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
        let method = TEXT.find("measure").unwrap();
        let name = TEXT.find(" value").unwrap() + 1;
        let start = name;
        let end = name + 5;
        let read = TEXT.rfind("value").unwrap();
        vec![
            fact(
                1,
                None,
                SyntaxFactKind::Declaration,
                span(0, TEXT.len()),
                "ring.method.declaration",
            ),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Occurrence,
                span(method, method + 7),
                "ring.name.definition",
            ),
            fact(
                3,
                Some(1),
                SyntaxFactKind::Declaration,
                span(start, end),
                "ring.parameter.declaration",
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Occurrence,
                span(name, name + 5),
                "ring.name.definition",
            ),
            fact(
                5,
                Some(1),
                SyntaxFactKind::Occurrence,
                span(read, read + 5),
                "ring.parameter_read.reference",
            ),
            fact(
                6,
                None,
                SyntaxFactKind::Signature,
                span(start, read + 5),
                "ring.parameter_read_target.signature",
            ),
        ]
    }

    fn materialized(
        kind: EntityKind,
        name: &str,
        parent: ContainerRef,
        range: SourceSpan,
        definition: u64,
    ) -> MaterializedEntity {
        let source = source();
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: source.repository(),
            language: "ring".to_owned(),
            kind,
            container: Some(parent),
            container_identity: b"native-method-owner".to_vec(),
            declared_identity: name.to_owned(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: content_hash(b"ring-source-control").as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        let record = EntityRecord {
            id: claim.symbol,
            repository: source.repository(),
            generation: source.generation(),
            kind,
            language: "ring".to_owned(),
            tier: AnalysisTier::TierD,
            canonical_name: name.to_owned(),
            display_name: name.to_owned(),
            qualified_name: name.to_owned(),
            container: Some(parent),
            visibility: EntityVisibility::Unknown,
            flags: Vec::new(),
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: Some(SourceRef::new(
                    source.repository(),
                    source.generation(),
                    range,
                    source.content_hash(),
                    None,
                )),
                derivation: Vec::new(),
            },
        };
        MaterializedEntity {
            record,
            identity_claim: claim,
            direct_parent: parent,
            identity_guard: [0; 32],
            definition_local_id: Some(definition),
            signature_evidence: None,
            signature_span: None,
        }
    }

    fn entities() -> HashMap<u64, MaterializedEntity> {
        let facts = witnesses();
        let class = materialized(
            EntityKind::Class,
            "Written",
            ContainerRef::File(source().span().file()),
            source().span(),
            8,
        );
        let method = materialized(
            EntityKind::Method,
            "measure",
            ContainerRef::Entity(class.record.id),
            facts[0].span(),
            2,
        );
        let parameter = materialized(
            EntityKind::Parameter,
            "value",
            ContainerRef::Entity(method.record.id),
            facts[2].span(),
            4,
        );
        HashMap::from([(1, method), (3, parameter), (7, class)])
    }

    fn plan(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        frame: &SourceRef,
    ) -> ReadTargets {
        ReadTargets::new(
            facts,
            entities,
            frame,
            |span| {
                Ok(&TEXT[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()])
            },
            Ok,
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn exact_parameter_method_definition_read_and_marker_are_jointly_required() {
        let original = witnesses();
        assert_eq!(plan(&original, &entities(), &source()).bindings.len(), 1);
        for missing in 0..original.len() {
            let mut facts = original.clone();
            facts.remove(missing);
            assert!(
                plan(&facts, &entities(), &source()).bindings.is_empty(),
                "missing={missing}"
            );
        }
        for duplicate in 0..original.len() {
            let mut facts = original.clone();
            let prior = &original[duplicate];
            facts.push(fact(
                99,
                prior.parent(),
                prior.kind(),
                prior.span(),
                prior.syntax_kind().as_str(),
            ));
            assert!(
                plan(&facts, &entities(), &source()).bindings.is_empty(),
                "duplicate={duplicate}"
            );
        }
        for range in [
            span(0, TEXT.rfind("value").unwrap() + 5),
            span(TEXT.find("value").unwrap(), TEXT.len()),
        ] {
            let mut facts = original.clone();
            facts[5] = fact(
                6,
                None,
                SyntaxFactKind::Signature,
                range,
                "ring.parameter_read_target.signature",
            );
            assert!(plan(&facts, &entities(), &source()).bindings.is_empty());
        }
    }

    #[test]
    fn materialized_kind_owner_definition_and_current_frame_cannot_be_substituted() {
        let facts = witnesses();
        for mutation in 0..7 {
            let mut entities = entities();
            let parameter = entities.get_mut(&3).unwrap();
            match mutation {
                0 => parameter.record.kind = EntityKind::Field,
                1 => parameter.record.language = "other".to_owned(),
                2 => parameter.record.container = Some(ContainerRef::File(source().span().file())),
                3 => parameter.definition_local_id = Some(2),
                4 => parameter.record.generation = GenerationId::from_bytes([9; 20]),
                5 => parameter.record.canonical_name = "other".to_owned(),
                _ => {
                    parameter.record.evidence.source = Some(SourceRef::new(
                        source().repository(),
                        source().generation(),
                        facts[2].span(),
                        content_hash(b"changed"),
                        None,
                    ));
                }
            }
            assert!(
                plan(&facts, &entities, &source()).bindings.is_empty(),
                "mutation={mutation}"
            );
        }
        let stale = SourceRef::new(
            source().repository(),
            GenerationId::from_bytes([9; 20]),
            source().span(),
            source().content_hash(),
            None,
        );
        assert!(plan(&facts, &entities(), &stale).bindings.is_empty());
    }

    #[test]
    fn read_adoption_rechecks_role_span_hash_provenance_and_cancellation() {
        let facts = witnesses();
        let entities = entities();
        let targets = plan(&facts, &entities, &source());
        let read = &facts[4];
        let reference = SourceRef::new(
            source().repository(),
            source().generation(),
            read.span(),
            source().content_hash(),
            None,
        );
        let site = unresolved_occurrence(
            read,
            OccurrenceRole::Read,
            Some(entities[&1].record.id),
            FactId::from_bytes([4; 20]),
            rootlight_ir::Confidence::new(500).unwrap(),
            reference,
            "value",
        );
        let site = site.unwrap();
        assert_eq!(
            targets
                .resolve(read, read.span(), &site, &Cancellation::new())
                .unwrap(),
            Some((entities[&3].record.id, entities[&1].record.id))
        );
        let wrong_physical_span = span(
            usize::try_from(read.span().start_byte()).unwrap() - 1,
            usize::try_from(read.span().end_byte()).unwrap() - 1,
        );
        assert!(
            targets
                .resolve(read, wrong_physical_span, &site, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        for mutation in 0..4 {
            let mut changed = site.clone();
            match mutation {
                0 => changed.role = OccurrenceRole::Reference,
                1 => changed.syntactic_text_hash = content_hash(b"other"),
                2 => changed.provenance = FactId::from_bytes([7; 20]),
                _ => changed.generation = GenerationId::from_bytes([9; 20]),
            }
            assert!(
                targets
                    .resolve(read, read.span(), &changed, &Cancellation::new())
                    .unwrap()
                    .is_none()
            );
        }
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            targets.resolve(read, read.span(), &site, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
