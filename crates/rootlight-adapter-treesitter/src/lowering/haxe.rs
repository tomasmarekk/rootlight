//! Bind Haxe argument Reads only with an exact native local-target certificate.
//! The consumer rechecks the Variable, its Method owner and current source frame.

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
        (EntityKind::Class, "haxe.class.declaration")
            | (EntityKind::Method, "haxe.method.declaration")
            | (EntityKind::Variable, "haxe.variable.declaration")
    )
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "haxe.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "haxe.local_read.reference" | "haxe.unresolved_read.reference",
        ) => Some(OccurrenceRole::Read),
        (SyntaxFactKind::Comment, "haxe.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "haxe.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "haxe.file.module") => &[
            (
                FactDomain::Entities,
                "haxe-fields-parameters-nested-binders-and-general-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "haxe-general-expression-writes-and-type-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "haxe-imports-types-conditional-state-macro-expansion-and-general-binding-unavailable",
            ),
        ],
        (SyntaxFactKind::Declaration, "haxe.unsupported.declaration") => &[
            (
                FactDomain::Entities,
                "haxe-written-declaration-identity-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "haxe-written-definition-unavailable",
            ),
            (FactDomain::Relations, "haxe-written-owner-unavailable"),
        ],
        (SyntaxFactKind::Signature, "haxe.macro_unavailable.expression") => &[
            (
                FactDomain::Entities,
                "haxe-quoted-macro-and-splice-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "haxe-quoted-macro-and-splice-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "haxe-macro-expansion-and-runtime-effects-unavailable",
            ),
        ],
        (SyntaxFactKind::Signature, "haxe.conditional_unavailable.expression") => &[
            (
                FactDomain::Entities,
                "haxe-conditional-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "haxe-conditional-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "haxe-conditional-branch-selection-unavailable",
            ),
        ],
        (SyntaxFactKind::Signature, "haxe.construct_unavailable.expression") => &[
            (
                FactDomain::Entities,
                "haxe-source-construct-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "haxe-source-construct-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "haxe-source-construct-semantics-unavailable",
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
            if fact.span().file() != source.span().file()
                || fact.span().start_byte() < source.span().start_byte()
                || fact.span().end_byte() > source.span().end_byte()
            {
                continue;
            }
            if fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == "haxe.variable.declaration"
            {
                unique(&mut declarations, fact.span().start_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == "haxe.method.declaration"
            {
                unique(&mut methods, fact.span().start_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "haxe.name.definition"
                && let Some(parent) = fact.parent()
            {
                unique(&mut definitions, parent, fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "haxe.local_read.reference"
            {
                unique(&mut reads, fact.span().end_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "haxe.local_read_target.signature"
            {
                unique(&mut markers, fact.span().end_byte(), fact);
            }
        }
        let mut bindings = HashMap::new();
        bindings
            .try_reserve(markers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for marker in markers.values().copied().flatten() {
            cancel.check()?;
            let Some(declaration) = declarations
                .get(&marker.span().start_byte())
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some(definition) = definitions.get(&declaration.local_id()).copied().flatten()
            else {
                continue;
            };
            let Some(read) = reads.get(&marker.span().end_byte()).copied().flatten() else {
                continue;
            };
            let Some(variable) = entities.get(&declaration.local_id()) else {
                continue;
            };
            let Some(variable_source) = variable.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(ContainerRef::Entity(method_id)) = variable.record.container else {
                continue;
            };
            let Some((method_local, method)) = by_symbol.get(&method_id).copied().flatten() else {
                continue;
            };
            let Some(method_source) = method.record.evidence.source.as_ref() else {
                continue;
            };
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
            if variable.record.language != "haxe"
                || variable.record.kind != EntityKind::Variable
                || method.record.language != "haxe"
                || method.record.kind != EntityKind::Method
                || variable.record.repository != source.repository()
                || variable.record.generation != source.generation()
                || method.record.repository != source.repository()
                || method.record.generation != source.generation()
                || variable.definition_local_id != Some(definition.local_id())
                || method.definition_local_id != Some(method_definition.local_id())
                || !same_source(variable_source, source)
                || !same_source(method_source, source)
                || variable_source.span() != declaration.span()
                || method_declaration.local_id() != method_local
                || method_declaration.span() != method_source.span()
                || text(method_definition.span())? != method.record.canonical_name
                || definition.span().start_byte() < declaration.span().start_byte()
                || definition.span().end_byte() > declaration.span().end_byte()
                || method_definition.span().start_byte() < method_source.span().start_byte()
                || method_definition.span().end_byte() > method_source.span().end_byte()
                || declaration.span().start_byte() < method_source.span().start_byte()
                || read.span().end_byte() > method_source.span().end_byte()
                || read.span().start_byte() < declaration.span().end_byte()
                || text(definition.span())? != variable.record.canonical_name
                || text(read.span())? != variable.record.canonical_name
            {
                continue;
            }
            bindings.insert(
                read.local_id(),
                Binding {
                    target: variable.record.id,
                    method: method_id,
                    span: read.span(),
                    spelling: content_hash(text(read.span())?.as_bytes()),
                    provenance: variable.record.provenance,
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
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<(SymbolId, SymbolId)>, AdapterError> {
        cancel.check()?;
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "haxe.local_read.reference"
            || site.syntax_kind != "haxe.local_read.reference"
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
                binding.span == fact.span()
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

    const TEXT: &str = "function measure() { var value = 1; consume(value); }";

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
        let start = TEXT.find("var value").unwrap();
        let name = start + 4;
        let end = TEXT.find(';').unwrap() + 1;
        let read = TEXT.rfind("value").unwrap();
        vec![
            fact(
                1,
                None,
                SyntaxFactKind::Declaration,
                span(0, TEXT.len()),
                "haxe.method.declaration",
            ),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Occurrence,
                span(method, method + 7),
                "haxe.name.definition",
            ),
            fact(
                3,
                Some(1),
                SyntaxFactKind::Declaration,
                span(start, end),
                "haxe.variable.declaration",
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Occurrence,
                span(name, name + 5),
                "haxe.name.definition",
            ),
            fact(
                5,
                Some(1),
                SyntaxFactKind::Occurrence,
                span(read, read + 5),
                "haxe.local_read.reference",
            ),
            fact(
                6,
                None,
                SyntaxFactKind::Signature,
                span(start, read + 5),
                "haxe.local_read_target.signature",
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
            language: "haxe".to_owned(),
            kind,
            container: Some(parent),
            container_identity: b"native-method-owner".to_vec(),
            declared_identity: name.to_owned(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: content_hash(b"haxe-source-control").as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        let record = EntityRecord {
            id: claim.symbol,
            repository: source.repository(),
            generation: source.generation(),
            kind,
            language: "haxe".to_owned(),
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
        let method = materialized(
            EntityKind::Method,
            "measure",
            ContainerRef::File(source().span().file()),
            facts[0].span(),
            2,
        );
        let variable = materialized(
            EntityKind::Variable,
            "value",
            ContainerRef::Entity(method.record.id),
            facts[2].span(),
            4,
        );
        HashMap::from([(1, method), (3, variable)])
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
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn exact_variable_method_definition_read_and_marker_are_jointly_required() {
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
            span(TEXT.find("var").unwrap(), TEXT.len()),
        ] {
            let mut facts = original.clone();
            facts[5] = fact(
                6,
                None,
                SyntaxFactKind::Signature,
                range,
                "haxe.local_read_target.signature",
            );
            assert!(plan(&facts, &entities(), &source()).bindings.is_empty());
        }
    }

    #[test]
    fn materialized_kind_owner_definition_and_current_frame_cannot_be_substituted() {
        let facts = witnesses();
        for mutation in 0..7 {
            let mut entities = entities();
            let variable = entities.get_mut(&3).unwrap();
            match mutation {
                0 => variable.record.kind = EntityKind::Field,
                1 => variable.record.language = "other".to_owned(),
                2 => variable.record.container = Some(ContainerRef::File(source().span().file())),
                3 => variable.definition_local_id = Some(2),
                4 => variable.record.generation = GenerationId::from_bytes([9; 20]),
                5 => variable.record.canonical_name = "other".to_owned(),
                _ => {
                    variable.record.evidence.source = Some(SourceRef::new(
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
            targets.resolve(read, &site, &Cancellation::new()).unwrap(),
            Some((entities[&3].record.id, entities[&1].record.id))
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
                    .resolve(read, &changed, &Cancellation::new())
                    .unwrap()
                    .is_none()
            );
        }
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            targets.resolve(read, &site, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
