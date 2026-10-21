//! Consume closed Agda RHS certificates against singular current raw identities.
//! Function signature/equation joins remain separate from this Reference facet;
//! no elaboration or application Calls are supplied by written spelling.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::{MaterializedEntity, agda, source_for_span, span_contains};

pub(super) const GAP: &str = "agda-written-rhs-binding-and-application-semantics-partial";

pub(super) fn reference(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == "agda.rhs.reference"
}

fn unique<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|old| *old = None)
        .or_insert(Some(value));
}

fn hash(text: &str, cancel: &Cancellation) -> Result<ContentHash, AdapterError> {
    let mut digest = blake3::Hasher::new();
    for chunk in text.as_bytes().chunks(64) {
        cancel.check()?;
        digest.update(chunk);
    }
    Ok(ContentHash::from_bytes(*digest.finalize().as_bytes()))
}

fn equal(left: &str, right: &str, cancel: &Cancellation) -> Result<bool, AdapterError> {
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

fn equation_prefix(text: &str, cancel: &Cancellation) -> Result<bool, AdapterError> {
    let mut equals = false;
    for chunk in text.as_bytes().chunks(64) {
        cancel.check()?;
        for &byte in chunk {
            if byte == b'=' && !equals {
                equals = true;
            } else if !byte.is_ascii_whitespace() {
                return Ok(false);
            }
        }
    }
    Ok(equals)
}

fn ordinary_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'%' | b'\'')
}

fn signature_excludes(text: &str, name: &str, cancel: &Cancellation) -> Result<bool, AdapterError> {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if at.is_multiple_of(64) {
            cancel.check()?;
        }
        if !ordinary_byte(bytes[at]) {
            at += 1;
            continue;
        }
        let begin = at;
        while at < bytes.len() && ordinary_byte(bytes[at]) {
            if at.is_multiple_of(64) {
                cancel.check()?;
            }
            at += 1;
        }
        if equal(&text[begin..at], name, cancel)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn closed_tail(text: &str, cancel: &Cancellation) -> Result<bool, AdapterError> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut depth = 0_usize;
    let mut argument = false;
    let mut needs_atom = false;
    while at < bytes.len() {
        if at.is_multiple_of(64) {
            cancel.check()?;
        }
        match bytes[at] {
            byte if byte.is_ascii_whitespace() => at += 1,
            b'(' => {
                depth += 1;
                needs_atom = true;
                at += 1;
            }
            b')' if depth > 0 && !needs_atom => {
                depth -= 1;
                at += 1;
            }
            byte if byte.is_ascii_alphabetic() => {
                let begin = at;
                while at < bytes.len() && ordinary_byte(bytes[at]) {
                    if at.is_multiple_of(64) {
                        cancel.check()?;
                    }
                    at += 1;
                }
                if matches!(
                    &bytes[begin..at],
                    b"let"
                        | b"where"
                        | b"do"
                        | b"with"
                        | b"rewrite"
                        | b"in"
                        | b"syntax"
                        | b"open"
                        | b"module"
                ) {
                    return Ok(false);
                }
                argument = true;
                needs_atom = false;
            }
            _ => return Ok(false),
        }
    }
    Ok(argument && depth == 0 && !needs_atom)
}

struct Binding {
    span: SourceSpan,
    owner: SymbolId,
    target: SymbolId,
    provenance: FactId,
    spelling: ContentHash,
}

pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl Targets {
    pub(super) fn new<'text>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        signatures: &agda::SignatureNames,
        source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut all = HashMap::new();
        let mut occupancy = HashMap::new();
        let mut definitions = HashMap::new();
        let mut signature_names = HashMap::new();
        let mut headers = HashMap::new();
        let mut markers = HashMap::new();
        let mut names = HashMap::new();
        let mut bindings = HashMap::new();
        definitions
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        signature_names
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        headers
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        markers
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        all.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        occupancy
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        bindings
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut root = None;
        let mut environment = None;
        let mut invalid = false;
        for fact in facts {
            cancel.check()?;
            unique(&mut all, fact.local_id(), fact);
            unique(&mut occupancy, (fact.kind(), fact.span()), fact.local_id());
            invalid |= !span_contains(source.span(), fact.span());
            match fact.kind() {
                SyntaxFactKind::Root => {
                    invalid |= root.replace(fact).is_some()
                        || fact.parent().is_some()
                        || fact.syntax_kind().as_str() != "agda.source_file"
                        || fact.span() != source.span();
                }
                SyntaxFactKind::Module
                | SyntaxFactKind::Import
                | SyntaxFactKind::ErrorRecovery
                | SyntaxFactKind::Scope => invalid = true,
                SyntaxFactKind::Signature
                    if fact.syntax_kind().as_str() == "agda.rhs.environment.signature" =>
                {
                    invalid |= environment.replace(fact).is_some() || fact.span() != source.span();
                }
                SyntaxFactKind::Signature
                    if fact.syntax_kind().as_str() == "agda.rhs.target.signature" =>
                {
                    unique(&mut markers, fact.span().end_byte(), fact);
                }
                SyntaxFactKind::Occurrence
                    if fact.syntax_kind().as_str() == "agda.function.definition" =>
                {
                    unique(&mut definitions, fact.parent(), fact);
                }
                SyntaxFactKind::Occurrence if agda::signature_name(fact) => {
                    unique(&mut signature_names, fact.parent(), fact);
                }
                SyntaxFactKind::Signature
                    if fact.syntax_kind().as_str() == "agda.function.signature" =>
                {
                    unique(&mut headers, fact.parent(), fact);
                }
                SyntaxFactKind::Declaration
                    if fact.syntax_kind().as_str() == "agda.function.declaration" => {}
                SyntaxFactKind::Occurrence if reference(fact) => {}
                _ => invalid = true,
            }
        }
        let empty = |bindings| Self {
            source: source.clone(),
            bindings,
        };
        let (Some(root), Some(environment)) = (root, environment) else {
            return Ok(empty(bindings));
        };
        for value in all.values() {
            cancel.check()?;
            invalid |= value.is_none();
        }
        if invalid
            || environment.parent() != Some(root.local_id())
            || occupancy.get(&(SyntaxFactKind::Signature, environment.span()))
                != Some(&Some(environment.local_id()))
        {
            return Ok(empty(bindings));
        }
        let mut by_id = HashMap::new();
        by_id
            .try_reserve(all.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (id, fact) in all {
            cancel.check()?;
            if let Some(fact) = fact {
                by_id.insert(id, fact);
            }
        }
        let mut validated = HashMap::new();
        validated
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::Declaration)
        {
            cancel.check()?;
            if fact.syntax_kind().as_str() != "agda.function.declaration"
                || fact.parent() != Some(root.local_id())
            {
                return Ok(empty(bindings));
            }
            let Some(name) = signature_names
                .get(&Some(fact.local_id()))
                .copied()
                .flatten()
            else {
                return Ok(empty(bindings));
            };
            let Some(entity) = signatures.target(name, &by_id, entities) else {
                return Ok(empty(bindings));
            };
            let Some(definition) = definitions.get(&Some(fact.local_id())).copied().flatten()
            else {
                return Ok(empty(bindings));
            };
            let Some(header) = headers.get(&Some(fact.local_id())).copied().flatten() else {
                return Ok(empty(bindings));
            };
            if entity.record.kind != EntityKind::Function
                || entity.record.language != "agda"
                || entity.record.repository != source.repository()
                || entity.record.generation != source.generation()
                || entity.direct_parent != ContainerRef::File(source.span().file())
                || entity.record.container != Some(entity.direct_parent)
                || entity.record.evidence.source.as_ref()
                    != Some(&source_for_span(source, fact.span()))
                || occupancy.get(&(SyntaxFactKind::Declaration, fact.span()))
                    != Some(&Some(fact.local_id()))
                || occupancy.get(&(SyntaxFactKind::Occurrence, definition.span()))
                    != Some(&Some(definition.local_id()))
                || occupancy.get(&(SyntaxFactKind::Occurrence, name.span()))
                    != Some(&Some(name.local_id()))
                || occupancy.get(&(SyntaxFactKind::Signature, header.span()))
                    != Some(&Some(header.local_id()))
                || !equal(
                    text(definition.span())?,
                    &entity.record.canonical_name,
                    cancel,
                )?
            {
                return Ok(empty(bindings));
            }
            unique(
                &mut names,
                hash(text(definition.span())?, cancel)?,
                fact.local_id(),
            );
            validated.insert(fact.local_id(), (entity, fact, definition));
        }
        for entity in entities.values() {
            cancel.check()?;
            if entity.record.kind == EntityKind::Module {
                return Ok(empty(bindings));
            }
        }
        for fact in facts.iter().filter(|fact| reference(fact)) {
            cancel.check()?;
            let Some(owner_id) = fact.parent() else {
                continue;
            };
            let Some((owner, declaration, owner_definition)) = validated.get(&owner_id) else {
                continue;
            };
            let Some(marker) = markers.get(&fact.span().end_byte()).copied().flatten() else {
                continue;
            };
            let Some(target_id) = names
                .get(&hash(text(fact.span())?, cancel)?)
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some((target, target_declaration, target_definition)) = validated.get(&target_id)
            else {
                continue;
            };
            if !span_contains(declaration.span(), fact.span())
                || fact.span().start_byte() < owner_definition.span().end_byte()
            {
                continue;
            }
            let prefix = SourceSpan::new(
                fact.span().file(),
                owner_definition.span().end_byte(),
                fact.span().start_byte(),
            )
            .map_err(|_| SinkError::InvalidIr)?;
            let Some(header) = owner.signature_span else {
                continue;
            };
            let tail = SourceSpan::new(
                fact.span().file(),
                fact.span().end_byte(),
                declaration.span().end_byte(),
            )
            .map_err(|_| SinkError::InvalidIr)?;
            if target_id == owner_id
                || marker.parent() != Some(root.local_id())
                || marker.span().start_byte() != target_definition.span().start_byte()
                || target_declaration.span().end_byte() > declaration.span().start_byte()
                || occupancy.get(&(SyntaxFactKind::Occurrence, fact.span()))
                    != Some(&Some(fact.local_id()))
                || occupancy.get(&(SyntaxFactKind::Signature, marker.span()))
                    != Some(&Some(marker.local_id()))
                || owner.record.provenance != target.record.provenance
                || !equation_prefix(text(prefix)?, cancel)?
                || !signature_excludes(text(header)?, text(fact.span())?, cancel)?
                || !closed_tail(text(tail)?, cancel)?
                || !equal(text(fact.span())?, text(target_definition.span())?, cancel)?
            {
                continue;
            }
            bindings.insert(
                fact.local_id(),
                Binding {
                    span: fact.span(),
                    owner: owner.record.id,
                    target: target.record.id,
                    provenance: owner.record.provenance,
                    spelling: hash(text(fact.span())?, cancel)?,
                },
            );
        }
        Ok(empty(bindings))
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        if !reference(fact)
            || site.syntax_kind != fact.syntax_kind().as_str()
            || site.role != OccurrenceRole::Reference
            || site.enclosing != Some(binding.owner)
            || site.provenance != binding.provenance
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
            || site.source != source_for_span(&self.source, binding.span)
            || site.syntactic_text_hash != binding.spelling
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
        {
            return Ok(None);
        }
        Ok(Some(binding.target))
    }
}
