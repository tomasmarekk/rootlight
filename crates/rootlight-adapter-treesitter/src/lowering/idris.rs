//! Join a native direct RHS certificate to a unique written global equation.
//! Signature elaboration, lambda binding and application evaluation stay unproved.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    SourceRef, SourceSpan,
};

use super::MaterializedEntity;

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    kind == EntityKind::Function && label == "idris.function.declaration"
}
pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "idris.own_body_global.reference" | "idris.unproved_name.reference"
        )
}
pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str().starts_with("idris.")
}
pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "idris.own_body_global.reference" | "idris.unproved_name.reference",
        ) => Some(OccurrenceRole::Reference),
        (SyntaxFactKind::Comment, "idris.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "idris.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    required(fact).then_some("idris-written-name-local-or-dependent-binding-unqualified")
}
pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "idris.file.root") => Some(
            "idris-only-plain-no-formal-equations-qualified-local-pattern-literate-import-and-dependent-elaboration-unqualified",
        ),
        (SyntaxFactKind::Root, "idris.native_context_unavailable.root")
        | (SyntaxFactKind::Signature, "idris.native_context_unavailable.signature") => {
            Some("idris-complete-clean-native-context-unavailable")
        }
        (SyntaxFactKind::Signature, "idris.equation_unavailable.signature") => {
            Some("idris-plain-unique-no-formal-equation-unavailable")
        }
        _ => None,
    };
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| detail.map(|detail| (domain, detail)))
    .chain(
        (fact.kind() == SyntaxFactKind::Root && fact.syntax_kind().as_str() == "idris.file.root")
            .then_some((
                FactDomain::Relations,
                "idris-application-call-normalization-unqualified",
            )),
    )
}
fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}
fn name(text: &str, cancel: &Cancellation) -> Result<Option<ContentHash>, AdapterError> {
    cancel.check()?;
    if !text
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
    {
        return Ok(None);
    }
    let mut hasher = blake3::Hasher::new();
    for chunk in text.as_bytes().chunks(64) {
        cancel.check()?;
        if !chunk
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\''))
        {
            return Ok(None);
        }
        hasher.update(chunk);
    }
    Ok(Some(ContentHash::from_bytes(*hasher.finalize().as_bytes())))
}
fn same_name(left: &str, right: &str, cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.as_bytes().chunks(64).zip(right.as_bytes().chunks(64)) {
        cancel.check()?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}
// The native certificate admits only a flat sequence of written names.
// Rechecking the complete current RHS prevents a moved witness from binding a lambda.
fn plain_body(text: &str, cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    enum State {
        Prefix,
        AfterEquals,
        Between,
        Name,
    }
    let mut state = State::Prefix;
    let mut saw_name = false;
    for chunk in text.as_bytes().chunks(64) {
        cancel.check()?;
        for byte in chunk {
            state = match state {
                State::Prefix if byte.is_ascii_whitespace() => State::Prefix,
                State::Prefix if *byte == b'=' => State::AfterEquals,
                State::AfterEquals if byte.is_ascii_whitespace() => State::Between,
                State::Between if byte.is_ascii_whitespace() => State::Between,
                State::Between if byte.is_ascii_alphabetic() || *byte == b'_' => {
                    saw_name = true;
                    State::Name
                }
                State::Name if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\'') => {
                    State::Name
                }
                State::Name if byte.is_ascii_whitespace() => State::Between,
                _ => return Ok(false),
            };
        }
    }
    Ok(saw_name && matches!(state, State::Name | State::Between))
}

fn token_boundary(body: &str, body_start: u64, site: SourceSpan) -> bool {
    let Some(start) = site
        .start_byte()
        .checked_sub(body_start)
        .and_then(|at| usize::try_from(at).ok())
    else {
        return false;
    };
    let Some(end) = site
        .end_byte()
        .checked_sub(body_start)
        .and_then(|at| usize::try_from(at).ok())
    else {
        return false;
    };
    start < end
        && end <= body.len()
        && start
            .checked_sub(1)
            .and_then(|at| body.as_bytes().get(at))
            .is_some_and(u8::is_ascii_whitespace)
        && (end == body.len()
            || body
                .as_bytes()
                .get(end)
                .is_some_and(u8::is_ascii_whitespace))
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|old| *old = None)
        .or_insert(Some(value));
}
struct Binding {
    symbol: SymbolId,
    owner: SymbolId,
    source: SourceSpan,
    fact: SourceSpan,
    provenance: FactId,
    name: ContentHash,
}
pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl Targets {
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        full: &SourceRef,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut out = Self {
            source: full.clone(),
            bindings: HashMap::new(),
        };
        let mut by_id = HashMap::new();
        let mut definitions = HashMap::new();
        let mut signatures = HashMap::new();
        let mut roots = Vec::new();
        let mut modules = Vec::new();
        let mut declarations = Vec::new();
        let mut scopes = HashSet::new();
        let mut opaque = false;
        let mut markers = HashMap::new();
        let mut references = HashMap::new();
        for map in [&mut markers, &mut references] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        for map in [&mut definitions, &mut signatures] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        scopes
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        declarations
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        roots
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        modules
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            if fact.span().file() != full.span().file()
                || by_id.insert(fact.local_id(), fact).is_some()
            {
                return Ok(out);
            }
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Root, "idris.file.root") if roots.is_empty() => roots.push(fact),
                (SyntaxFactKind::Module, "idris.file.module") if modules.is_empty() => {
                    modules.push(fact)
                }
                (SyntaxFactKind::Scope, "idris.function.scope") => {
                    scopes.insert(fact.local_id());
                }
                (SyntaxFactKind::Declaration, "idris.function.declaration") => {
                    declarations.push(fact)
                }
                (SyntaxFactKind::Occurrence, "idris.function_name.definition") => {
                    let Some(parent) = fact.parent() else {
                        return Ok(out);
                    };
                    unique(&mut definitions, parent, fact);
                }
                (SyntaxFactKind::Signature, "idris.function.signature") => {
                    let Some(parent) = fact.parent() else {
                        return Ok(out);
                    };
                    unique(&mut signatures, parent, fact);
                }
                (SyntaxFactKind::Occurrence, "idris.own_body_global.reference") => {
                    let key = (
                        fact.parent(),
                        fact.span().start_byte(),
                        fact.span().end_byte(),
                    );
                    unique(&mut references, key, fact);
                }
                (SyntaxFactKind::Signature, "idris.direct_global_context.signature") => {
                    let key = (
                        fact.parent(),
                        fact.span().start_byte(),
                        fact.span().end_byte(),
                    );
                    unique(&mut markers, key, fact);
                }
                (SyntaxFactKind::Occurrence, "idris.unproved_name.reference")
                | (SyntaxFactKind::Signature, "idris.type_signature.signature")
                | (SyntaxFactKind::Comment, "idris.comment.comment")
                | (SyntaxFactKind::StringLiteral, "idris.string.string") => {}
                _ => opaque = true,
            }
        }
        let ([root], [module]) = (roots.as_slice(), modules.as_slice()) else {
            return Ok(out);
        };
        for (key, reference) in &references {
            cancel.check()?;
            if reference.is_none() || !markers.get(key).is_some_and(Option::is_some) {
                return Ok(out);
            }
        }
        if opaque
            || root.parent().is_some()
            || module.parent() != Some(root.local_id())
            || physical(root.span())? != full.span()
            || physical(module.span())? != full.span()
            || definitions.len() != declarations.len()
            || signatures.len() != declarations.len()
            || scopes.len() != declarations.len()
            || markers.len() != references.len()
        {
            return Ok(out);
        }
        let Some(module_entity) = entities.get(&module.local_id()) else {
            return Ok(out);
        };
        if module_entity.record.kind != EntityKind::Module
            || module_entity.record.language != "idris"
            || module_entity.direct_parent != ContainerRef::File(full.span().file())
            || module_entity.record.container != Some(module_entity.direct_parent)
            || module_entity.definition_local_id.is_some()
            || module_entity.record.repository != full.repository()
            || module_entity.record.generation != full.generation()
            || !module_entity
                .record
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| same_source(source, full) && source.span() == full.span())
        {
            return Ok(out);
        }
        let mut names = HashMap::new();
        let mut owners = HashMap::new();
        let mut used_scopes = HashSet::new();
        let mut bodies = HashMap::new();
        bodies
            .try_reserve(declarations.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        names
            .try_reserve(declarations.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        owners
            .try_reserve(declarations.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        used_scopes
            .try_reserve(declarations.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for declaration in declarations {
            cancel.check()?;
            let Some(scope) = declaration.parent().and_then(|id| by_id.get(&id)).copied() else {
                return Ok(out);
            };
            let Some(definition) = definitions.get(&declaration.local_id()).copied().flatten()
            else {
                return Ok(out);
            };
            let Some(signature) = signatures.get(&declaration.local_id()).copied().flatten() else {
                return Ok(out);
            };
            let Some(entity) = entities.get(&declaration.local_id()) else {
                return Ok(out);
            };
            let spelling = text(definition.span())?;
            let Some(digest) = name(spelling, cancel)? else {
                return Ok(out);
            };
            if !scopes.contains(&scope.local_id())
                || !used_scopes.insert(scope.local_id())
                || scope.parent() != Some(module.local_id())
                || scope.span() != declaration.span()
                || signature.span() != declaration.span()
                || definition.span().start_byte() != declaration.span().start_byte()
                || definition.span().end_byte() > declaration.span().end_byte()
                || entity.record.language != "idris"
                || entity.record.kind != EntityKind::Function
                || entity.record.repository != full.repository()
                || entity.record.generation != full.generation()
                || entity.record.provenance != module_entity.record.provenance
                || entity.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || entity.record.container != Some(entity.direct_parent)
                || entity.definition_local_id != Some(definition.local_id())
                || !same_name(spelling, &entity.record.canonical_name, cancel)?
                || !entity
                    .record
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| same_source(source, full))
            {
                return Ok(out);
            }
            if entity.record.evidence.source.as_ref().map(SourceRef::span)
                != Some(physical(declaration.span())?)
            {
                return Ok(out);
            }
            let declaration_text = text(declaration.span())?;
            let definition_width = definition
                .span()
                .end_byte()
                .checked_sub(declaration.span().start_byte())
                .and_then(|width| usize::try_from(width).ok());
            let body = definition_width.and_then(|width| declaration_text.get(width..));
            if let Some(body) = body
                && plain_body(body, cancel)?
            {
                bodies.insert(declaration.local_id(), (definition.span().end_byte(), body));
            }
            unique(&mut names, digest, entity);
            owners.insert(declaration.local_id(), (declaration, entity, definition));
        }
        if used_scopes.len() != scopes.len() {
            return Ok(out);
        }
        for fact in facts {
            cancel.check()?;
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Signature, "idris.type_signature.signature")
                    if fact.parent() != Some(module.local_id()) =>
                {
                    return Ok(out);
                }
                (SyntaxFactKind::Occurrence, "idris.unproved_name.reference")
                | (SyntaxFactKind::Comment, "idris.comment.comment")
                | (SyntaxFactKind::StringLiteral, "idris.string.string")
                    if fact.parent() != Some(module.local_id())
                        && !fact.parent().is_some_and(|id| owners.contains_key(&id)) =>
                {
                    return Ok(out);
                }
                _ => {}
            }
        }
        out.bindings
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            if fact.kind() != SyntaxFactKind::Occurrence
                || fact.syntax_kind().as_str() != "idris.own_body_global.reference"
            {
                continue;
            }
            let Some((declaration, owner, definition)) =
                fact.parent().and_then(|id| owners.get(&id)).copied()
            else {
                return Ok(Self {
                    source: full.clone(),
                    bindings: HashMap::new(),
                });
            };
            let Some((body_start, body)) = bodies.get(&declaration.local_id()).copied() else {
                continue;
            };
            if !token_boundary(body, body_start, fact.span()) {
                continue;
            }
            let spelling = text(fact.span())?;
            let Some(digest) = name(spelling, cancel)? else {
                continue;
            };
            let Some(target) = names.get(&digest).copied().flatten() else {
                continue;
            };
            if fact.span().start_byte() < definition.span().end_byte()
                || fact.span().end_byte() > declaration.span().end_byte()
                || !same_name(spelling, &target.record.canonical_name, cancel)?
            {
                continue;
            }
            out.bindings.insert(
                fact.local_id(),
                Binding {
                    symbol: target.record.id,
                    owner: owner.record.id,
                    source: physical(fact.span())?,
                    fact: fact.span(),
                    provenance: target.record.provenance,
                    name: digest,
                },
            );
        }
        Ok(out)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "idris.own_body_global.reference"
            || site.syntax_kind != "idris.own_body_global.reference"
            || site.role != OccurrenceRole::Reference
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
                binding.fact == fact.span()
                    && binding.source == site.source.span()
                    && binding.provenance == site.provenance
                    && binding.name == site.syntactic_text_hash
                    && site.enclosing == Some(binding.owner)
            })
            .map(|binding| binding.symbol))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{FileId, GenerationId, RepositoryId, content_hash};
    use rootlight_ir::{
        AnalysisTier, EntityRecord, EntityVisibility, FactEvidence, SymbolIdentityClaim,
    };

    const TEXT: &str = "global : Type\nglobal = Type\nreader : Type\nreader = global\n";
    fn span(start: u64, end: u64) -> SourceSpan {
        SourceSpan::new(FileId::from_bytes([2; 20]), start, end).unwrap()
    }
    fn source(range: SourceSpan) -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([3; 20]),
            range,
            content_hash(TEXT.as_bytes()),
            None,
        )
    }
    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        start: u64,
        end: u64,
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            span(start, end),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }
    fn facts() -> Vec<SyntaxFact> {
        vec![
            fact(1, None, SyntaxFactKind::Root, 0, 58, "idris.file.root"),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Module,
                0,
                58,
                "idris.file.module",
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Signature,
                0,
                13,
                "idris.type_signature.signature",
            ),
            fact(
                4,
                Some(2),
                SyntaxFactKind::Scope,
                14,
                27,
                "idris.function.scope",
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Declaration,
                14,
                27,
                "idris.function.declaration",
            ),
            fact(
                6,
                Some(5),
                SyntaxFactKind::Occurrence,
                14,
                20,
                "idris.function_name.definition",
            ),
            fact(
                7,
                Some(5),
                SyntaxFactKind::Signature,
                14,
                27,
                "idris.function.signature",
            ),
            fact(
                8,
                Some(2),
                SyntaxFactKind::Signature,
                28,
                41,
                "idris.type_signature.signature",
            ),
            fact(
                9,
                Some(2),
                SyntaxFactKind::Scope,
                42,
                57,
                "idris.function.scope",
            ),
            fact(
                10,
                Some(9),
                SyntaxFactKind::Declaration,
                42,
                57,
                "idris.function.declaration",
            ),
            fact(
                11,
                Some(10),
                SyntaxFactKind::Occurrence,
                42,
                48,
                "idris.function_name.definition",
            ),
            fact(
                12,
                Some(10),
                SyntaxFactKind::Signature,
                42,
                57,
                "idris.function.signature",
            ),
            fact(
                13,
                Some(10),
                SyntaxFactKind::Occurrence,
                51,
                57,
                "idris.own_body_global.reference",
            ),
            fact(
                14,
                Some(10),
                SyntaxFactKind::Signature,
                51,
                57,
                "idris.direct_global_context.signature",
            ),
        ]
    }
    fn entity(
        local: u64,
        kind: EntityKind,
        name: &str,
        range: SourceSpan,
        definition: Option<u64>,
        owner: ContainerRef,
    ) -> MaterializedEntity {
        entity_with_source(
            local,
            kind,
            name,
            range,
            definition,
            owner,
            &source(span(0, 58)),
        )
    }

    fn entity_with_source(
        local: u64,
        kind: EntityKind,
        name: &str,
        range: SourceSpan,
        definition: Option<u64>,
        owner: ContainerRef,
        full: &SourceRef,
    ) -> MaterializedEntity {
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: full.repository(),
            language: "idris".to_owned(),
            kind,
            container: Some(owner),
            container_identity: vec![u8::try_from(local).unwrap()],
            declared_identity: name.to_owned(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: content_hash(b"idris-written-test").as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        MaterializedEntity {
            record: EntityRecord {
                id: claim.symbol,
                repository: full.repository(),
                generation: full.generation(),
                kind,
                language: "idris".to_owned(),
                tier: AnalysisTier::TierD,
                canonical_name: name.to_owned(),
                display_name: name.to_owned(),
                qualified_name: name.to_owned(),
                container: Some(owner),
                visibility: EntityVisibility::Unknown,
                flags: Vec::new(),
                provenance: FactId::from_bytes([4; 20]),
                evidence: FactEvidence {
                    source: Some(SourceRef::new(
                        full.repository(),
                        full.generation(),
                        range,
                        full.content_hash(),
                        None,
                    )),
                    derivation: Vec::new(),
                },
            },
            identity_claim: claim,
            direct_parent: owner,
            identity_guard: [0; 32],
            definition_local_id: definition,
            signature_evidence: None,
            signature_span: None,
        }
    }

    fn entities() -> HashMap<u64, MaterializedEntity> {
        let module = entity(
            2,
            EntityKind::Module,
            "written.idr",
            span(0, 58),
            None,
            ContainerRef::File(span(0, 58).file()),
        );
        let parent = ContainerRef::Entity(module.record.id);
        HashMap::from([
            (2, module),
            (
                5,
                entity(
                    5,
                    EntityKind::Function,
                    "global",
                    span(14, 27),
                    Some(6),
                    parent,
                ),
            ),
            (
                10,
                entity(
                    10,
                    EntityKind::Function,
                    "reader",
                    span(42, 57),
                    Some(11),
                    parent,
                ),
            ),
        ])
    }
    fn plan(facts: &[SyntaxFact], entities: &HashMap<u64, MaterializedEntity>) -> Targets {
        Targets::new(
            facts,
            entities,
            &source(span(0, 58)),
            Ok,
            |span| {
                Ok(&TEXT[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()])
            },
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn complete_native_chain_binds_one_reference_to_its_written_global_and_owner() {
        assert_eq!(TEXT.len(), 58);
        assert_eq!(&TEXT[14..20], "global");
        assert_eq!(&TEXT[51..57], "global");
        let entities = entities();
        let targets = plan(&facts(), &entities);
        assert_eq!(targets.bindings.len(), 1);
        let binding = &targets.bindings[&13];
        assert_eq!(binding.symbol, entities[&5].record.id);
        assert_eq!(binding.owner, entities[&10].record.id);
        assert_eq!(binding.source, span(51, 57));
        assert_eq!(binding.name, content_hash(b"global"));
    }

    #[test]
    fn foreign_cardinality_missing_markers_and_reparented_chains_cannot_bind() {
        let original = facts();
        let entities = entities();
        assert_eq!(plan(&original, &entities).bindings.len(), 1);
        for variant in 0..11 {
            let mut changed = original.clone();
            match variant {
                0 => changed.push(fact(
                    21,
                    None,
                    SyntaxFactKind::Root,
                    0,
                    58,
                    "idris.file.root",
                )),
                1 => changed.push(fact(
                    21,
                    Some(4),
                    SyntaxFactKind::Declaration,
                    14,
                    27,
                    "foreign.function.declaration",
                )),
                2 => changed.push(fact(
                    21,
                    Some(5),
                    SyntaxFactKind::Occurrence,
                    14,
                    20,
                    "foreign.name.definition",
                )),
                3 => {
                    changed[12] = fact(
                        13,
                        Some(2),
                        SyntaxFactKind::Occurrence,
                        51,
                        57,
                        "idris.own_body_global.reference",
                    )
                }
                4 => {
                    changed[9] = fact(
                        10,
                        Some(2),
                        SyntaxFactKind::Declaration,
                        42,
                        57,
                        "idris.function.declaration",
                    )
                }
                5 => {
                    changed[8] = fact(
                        9,
                        Some(1),
                        SyntaxFactKind::Scope,
                        42,
                        57,
                        "idris.function.scope",
                    )
                }
                6 => changed.push(fact(
                    21,
                    Some(10),
                    SyntaxFactKind::Signature,
                    42,
                    57,
                    "idris.function.signature",
                )),
                7 => {
                    changed[5] = fact(
                        6,
                        Some(4),
                        SyntaxFactKind::Occurrence,
                        14,
                        20,
                        "idris.function_name.definition",
                    )
                }
                8 => {
                    changed[12] = fact(
                        13,
                        Some(9),
                        SyntaxFactKind::Occurrence,
                        51,
                        57,
                        "idris.own_body_global.reference",
                    )
                }
                9 => {
                    changed.remove(11);
                }
                _ => changed.push(fact(
                    21,
                    Some(10),
                    SyntaxFactKind::Occurrence,
                    51,
                    57,
                    "foreign.name.reference",
                )),
            }
            assert!(
                plan(&changed, &entities).bindings.is_empty(),
                "variant {variant}"
            );
        }
    }

    #[test]
    fn foreign_entity_provenance_names_and_cancellation_cannot_supply_a_global() {
        let original = facts();
        assert_eq!(plan(&original, &entities()).bindings.len(), 1);
        for variant in 0..3 {
            let mut entities = entities();
            match variant {
                0 => entities.get_mut(&5).unwrap().record.provenance = FactId::from_bytes([9; 20]),
                1 => entities.get_mut(&5).unwrap().record.canonical_name = "another".to_owned(),
                _ => entities.get_mut(&2).unwrap().record.language = "other".to_owned(),
            }
            assert!(
                plan(&original, &entities).bindings.is_empty(),
                "entity variant {variant}"
            );
        }
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        let error = Targets::new(
            &original,
            &entities(),
            &source(span(0, 58)),
            Ok,
            |_| Ok("global"),
            &cancel,
        )
        .err()
        .unwrap();
        assert!(matches!(
            error,
            AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            }
        ));
    }

    #[test]
    fn missing_duplicate_and_moved_direct_body_markers_cannot_promote_unproved_names() {
        let original = facts();
        let entities = entities();
        assert_eq!(plan(&original, &entities).bindings.len(), 1);
        for variant in 0..5 {
            let mut changed = original.clone();
            match variant {
                0 => {
                    changed.pop();
                }
                1 => changed.push(fact(
                    20,
                    Some(10),
                    SyntaxFactKind::Signature,
                    51,
                    57,
                    "idris.direct_global_context.signature",
                )),
                2 => {
                    *changed.last_mut().unwrap() = fact(
                        14,
                        Some(5),
                        SyntaxFactKind::Signature,
                        51,
                        57,
                        "idris.direct_global_context.signature",
                    );
                }
                3 => {
                    *changed.last_mut().unwrap() = fact(
                        14,
                        Some(10),
                        SyntaxFactKind::Signature,
                        52,
                        57,
                        "idris.direct_global_context.signature",
                    );
                }
                _ => {
                    *changed.last_mut().unwrap() = fact(
                        14,
                        Some(10),
                        SyntaxFactKind::Occurrence,
                        51,
                        57,
                        "idris.direct_global_context.signature",
                    );
                }
            }
            assert!(
                plan(&changed, &entities).bindings.is_empty(),
                "marker variant {variant}"
            );
        }
        let cancel = Cancellation::new();
        for source in [" = global", " = global another\r\n", " = _value' "] {
            assert!(plain_body(source, &cancel).unwrap(), "{source:?}");
        }
        for source in [
            " = \\global => global",
            " = global + other",
            " = \"global\"",
            " == global",
            " = (global)",
            " = global -- comment",
            " = ",
            " = global\0",
        ] {
            assert!(!plain_body(source, &cancel).unwrap(), "{source:?}");
        }
        let source = include_str!("../../tests/fixtures/idris/stlc_small.idr");
        assert!(!plain_body(source.get(1218..1256).unwrap(), &cancel).unwrap());
        assert!(plain_body(source.get(1589..1598).unwrap(), &cancel).unwrap());
        assert!(token_boundary(
            source.get(1589..1598).unwrap(),
            1589,
            span(1592, 1595)
        ));
        assert!(!token_boundary(
            source.get(1589..1598).unwrap(),
            1589,
            span(1593, 1595)
        ));
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            plain_body(" = global", &cancel),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn original_lambda_relabel_and_repositioned_marker_cannot_bind_the_global() {
        let original = include_str!("../../tests/fixtures/idris/stlc_small.idr");
        assert_eq!(original.len(), 2167);
        assert_eq!(original.get(1247..1250), Some("var"));
        assert_eq!(original.get(1592..1595), Some("var"));
        let template = source(span(0, 2167));
        let full = SourceRef::new(
            template.repository(),
            template.generation(),
            template.span(),
            content_hash(original.as_bytes()),
            None,
        );
        let module = entity_with_source(
            2,
            EntityKind::Module,
            "written.idr",
            span(0, 2167),
            None,
            ContainerRef::File(full.span().file()),
            &full,
        );
        let parent = ContainerRef::Entity(module.record.id);
        let entities = HashMap::from([
            (2, module),
            (
                5,
                entity_with_source(
                    5,
                    EntityKind::Function,
                    "var",
                    span(1215, 1256),
                    Some(6),
                    parent,
                    &full,
                ),
            ),
            (
                10,
                entity_with_source(
                    10,
                    EntityKind::Function,
                    "v0",
                    span(1587, 1598),
                    Some(11),
                    parent,
                    &full,
                ),
            ),
        ]);
        let baseline = vec![
            fact(1, None, SyntaxFactKind::Root, 0, 2167, "idris.file.root"),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Module,
                0,
                2167,
                "idris.file.module",
            ),
            fact(
                4,
                Some(2),
                SyntaxFactKind::Scope,
                1215,
                1256,
                "idris.function.scope",
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Declaration,
                1215,
                1256,
                "idris.function.declaration",
            ),
            fact(
                6,
                Some(5),
                SyntaxFactKind::Occurrence,
                1215,
                1218,
                "idris.function_name.definition",
            ),
            fact(
                7,
                Some(5),
                SyntaxFactKind::Signature,
                1215,
                1256,
                "idris.function.signature",
            ),
            fact(
                9,
                Some(2),
                SyntaxFactKind::Scope,
                1587,
                1598,
                "idris.function.scope",
            ),
            fact(
                10,
                Some(9),
                SyntaxFactKind::Declaration,
                1587,
                1598,
                "idris.function.declaration",
            ),
            fact(
                11,
                Some(10),
                SyntaxFactKind::Occurrence,
                1587,
                1589,
                "idris.function_name.definition",
            ),
            fact(
                12,
                Some(10),
                SyntaxFactKind::Signature,
                1587,
                1598,
                "idris.function.signature",
            ),
            fact(
                13,
                Some(10),
                SyntaxFactKind::Occurrence,
                1592,
                1595,
                "idris.own_body_global.reference",
            ),
            fact(
                14,
                Some(10),
                SyntaxFactKind::Signature,
                1592,
                1595,
                "idris.direct_global_context.signature",
            ),
            fact(
                15,
                Some(5),
                SyntaxFactKind::Occurrence,
                1247,
                1250,
                "idris.unproved_name.reference",
            ),
        ];
        let make_plan = |facts: &[SyntaxFact]| {
            Targets::new(
                facts,
                &entities,
                &full,
                Ok,
                |range| {
                    Ok(&original[usize::try_from(range.start_byte()).unwrap()
                        ..usize::try_from(range.end_byte()).unwrap()])
                },
                &Cancellation::new(),
            )
            .unwrap()
        };
        let targets = make_plan(&baseline);
        assert_eq!(targets.bindings.len(), 1);
        assert_eq!(targets.bindings[&13].symbol, entities[&5].record.id);
        assert!(!targets.bindings.contains_key(&15));
        let mut relabelled = baseline.clone();
        *relabelled.last_mut().unwrap() = fact(
            15,
            Some(5),
            SyntaxFactKind::Occurrence,
            1247,
            1250,
            "idris.own_body_global.reference",
        );
        assert!(make_plan(&relabelled).bindings.is_empty());
        relabelled.push(fact(
            16,
            Some(5),
            SyntaxFactKind::Signature,
            1247,
            1250,
            "idris.direct_global_context.signature",
        ));
        let targets = make_plan(&relabelled);
        assert_eq!(targets.bindings.len(), 1);
        assert_eq!(targets.bindings[&13].symbol, entities[&5].record.id);
        assert!(!targets.bindings.contains_key(&15));
    }
}
