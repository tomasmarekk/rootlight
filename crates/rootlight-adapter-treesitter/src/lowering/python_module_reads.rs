//! Join native module-read windows to current written Python assignment slots.
//! Independent assignment/header lexers admit only written operand positions.
//! This proves lexical identity, never call authority, a value or execution.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::MaterializedEntity;

/// Keep certified uses in both required fact accounting and artifact replay.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "python.module_read.reference"
}

fn digest(text: &str, cancel: &Cancellation) -> Result<Option<ContentHash>, AdapterError> {
    cancel.check()?;
    if !text
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
    {
        return Ok(None);
    }
    let mut hash = blake3::Hasher::new();
    for chunk in text.as_bytes().chunks(64) {
        cancel.check()?;
        if !chunk
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            return Ok(None);
        }
        hash.update(chunk);
    }
    Ok(Some(ContentHash::from_bytes(*hash.finalize().as_bytes())))
}

fn equal(left: &str, right: &str, cancel: &Cancellation) -> Result<bool, AdapterError> {
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

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    *left == super::source_for_span(right, left.span())
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|old| *old = None)
        .or_insert(Some(value));
}

// This lexer independently restricts the current assignment text. In particular,
// a certificate moved into a lambda cannot inherit a native label's authority.
fn flat_names(
    text: &str,
    name_bytes: usize,
    cancel: &Cancellation,
) -> Result<Option<Vec<(usize, usize)>>, AdapterError> {
    let raw = text.as_bytes();
    let mut at = name_bytes;
    while raw.get(at).is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
        cancel.check()?;
        at += 1;
    }
    if raw.get(at) != Some(&b'=') || raw.get(at + 1) == Some(&b'=') {
        return Ok(None);
    }
    at += 1;
    expression_names(text, at, text.len(), cancel)
}

fn expression_names(
    text: &str,
    start: usize,
    end: usize,
    cancel: &Cancellation,
) -> Result<Option<Vec<(usize, usize)>>, AdapterError> {
    let Some(raw) = text.as_bytes().get(..end) else {
        return Ok(None);
    };
    let mut at = start;
    let mut names = Vec::new();
    let mut operand = true;
    let mut depth = 0_usize;
    while at < raw.len() {
        cancel.check()?;
        match raw[at] {
            b' ' | b'\t' => at += 1,
            b'a'..=b'z' | b'A'..=b'Z' | b'_' if operand => {
                let start = at;
                while raw
                    .get(at)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    cancel.check()?;
                    at += 1;
                }
                let word = &raw[start..at];
                if matches!(
                    word,
                    b"False"
                        | b"None"
                        | b"True"
                        | b"and"
                        | b"as"
                        | b"assert"
                        | b"async"
                        | b"await"
                        | b"break"
                        | b"class"
                        | b"continue"
                        | b"def"
                        | b"del"
                        | b"elif"
                        | b"else"
                        | b"except"
                        | b"finally"
                        | b"for"
                        | b"from"
                        | b"global"
                        | b"if"
                        | b"import"
                        | b"in"
                        | b"is"
                        | b"lambda"
                        | b"nonlocal"
                        | b"not"
                        | b"or"
                        | b"pass"
                        | b"raise"
                        | b"return"
                        | b"try"
                        | b"while"
                        | b"with"
                        | b"yield"
                ) {
                    return Ok(None);
                }
                names
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                names.push((start, at));
                operand = false;
            }
            b'0'..=b'9' if operand => {
                while raw.get(at).is_some_and(u8::is_ascii_digit) {
                    cancel.check()?;
                    at += 1;
                }
                if raw.get(at) == Some(&b'.') {
                    at += 1;
                    while raw.get(at).is_some_and(u8::is_ascii_digit) {
                        cancel.check()?;
                        at += 1;
                    }
                }
                if raw.get(at).is_some_and(|byte| matches!(byte, b'e' | b'E')) {
                    at += 1;
                    if raw.get(at).is_some_and(|byte| matches!(byte, b'+' | b'-')) {
                        at += 1;
                    }
                    let exponent = at;
                    while raw.get(at).is_some_and(u8::is_ascii_digit) {
                        cancel.check()?;
                        at += 1;
                    }
                    if at == exponent {
                        return Ok(None);
                    }
                }
                operand = false;
            }
            b'(' if operand => {
                depth += 1;
                at += 1;
            }
            b')' if !operand && depth > 0 => {
                depth -= 1;
                at += 1;
            }
            b'+' | b'-' if operand => at += 1,
            b'~' if operand => at += 1,
            b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' | b'<' | b'>' if !operand => {
                let operator = raw[at];
                at += 1;
                if matches!(operator, b'*' | b'/') && raw.get(at) == Some(&operator) {
                    at += 1;
                }
                if matches!(operator, b'<' | b'>') {
                    if raw.get(at) != Some(&operator) {
                        return Ok(None);
                    }
                    at += 1;
                }
                operand = true;
            }
            _ => return Ok(None),
        }
    }
    Ok((!operand && depth == 0).then_some(names))
}

fn ordinary_word(text: &str, at: &mut usize, cancel: &Cancellation) -> Result<bool, AdapterError> {
    let start = *at;
    while text
        .as_bytes()
        .get(*at)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        cancel.check()?;
        *at += 1;
    }
    Ok(expression_names(text, start, *at, cancel)?
        .is_some_and(|names| names.as_slice() == [(start, *at)]))
}

fn horizontal_space(raw: &[u8], at: &mut usize, cancel: &Cancellation) -> Result<(), AdapterError> {
    while raw
        .get(*at)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        cancel.check()?;
        *at += 1;
    }
    Ok(())
}

fn iterable_names(
    text: &str,
    cancel: &Cancellation,
) -> Result<Option<Vec<(usize, usize)>>, AdapterError> {
    let raw = text.as_bytes();
    if !raw.starts_with(b"for") || !raw.get(3).is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
        return Ok(None);
    }
    let mut at = 3;
    horizontal_space(raw, &mut at, cancel)?;
    if !ordinary_word(text, &mut at, cancel)?
        || !raw.get(at).is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        return Ok(None);
    }
    horizontal_space(raw, &mut at, cancel)?;
    if raw.get(at..).is_none_or(|tail| !tail.starts_with(b"in")) {
        return Ok(None);
    }
    at += 2;
    if !raw.get(at).is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
        return Ok(None);
    }
    horizontal_space(raw, &mut at, cancel)?;
    if !ordinary_word(text, &mut at, cancel)? {
        return Ok(None);
    }
    horizontal_space(raw, &mut at, cancel)?;
    if raw.get(at) != Some(&b'(') {
        return Ok(None);
    }
    at += 1;
    let mut begin = at;
    let mut depth = 0_usize;
    let mut names = Vec::new();
    let mut arguments = 0_usize;
    loop {
        cancel.check()?;
        let Some(byte) = raw.get(at).copied() else {
            return Ok(None);
        };
        match byte {
            b'(' => depth += 1,
            b')' if depth > 0 => depth -= 1,
            b',' | b')' if depth == 0 => {
                let empty = raw
                    .get(begin..at)
                    .is_some_and(|part| part.iter().all(|byte| matches!(byte, b' ' | b'\t')));
                if empty {
                    if byte != b')' || (arguments == 0 && begin != at) {
                        return Ok(None);
                    }
                } else {
                    let Some(words) = expression_names(text, begin, at, cancel)? else {
                        return Ok(None);
                    };
                    names
                        .try_reserve(words.len())
                        .map_err(|_| SinkError::AllocationFailed)?;
                    names.extend(words);
                    arguments += 1;
                }
                if byte == b')' {
                    at += 1;
                    break;
                }
                begin = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    horizontal_space(raw, &mut at, cancel)?;
    Ok((raw.get(at) == Some(&b':') && at + 1 == raw.len()).then_some(names))
}

// Without a whole-file lexer, multiline literals and continuations before a
// header remain unproved. One prefix limit also avoids rescanning per use.
fn iterable_prefix_limit(text: &str, cancel: &Cancellation) -> Result<usize, AdapterError> {
    let raw = text.as_bytes();
    let mut quote = None;
    let mut comment = false;
    for (chunk_index, chunk) in raw.chunks(64).enumerate() {
        cancel.check()?;
        let start = chunk_index * 64;
        for (offset, byte) in chunk.iter().enumerate() {
            let at = start + offset;
            if comment {
                if *byte == b'\n' {
                    comment = false;
                }
                continue;
            }
            if quote.is_none() && *byte == b'#' {
                comment = true;
                continue;
            }
            let tail = &raw[at..];
            let quoted = matches!(byte, b'\'' | b'"');
            let f_string = quote.is_none()
                && quoted
                && (at
                    .checked_sub(1)
                    .and_then(|index| raw.get(index))
                    .is_some_and(|byte| matches!(byte, b'f' | b'F'))
                    || (at
                        .checked_sub(1)
                        .and_then(|index| raw.get(index))
                        .is_some_and(|byte| matches!(byte, b'r' | b'R'))
                        && at
                            .checked_sub(2)
                            .and_then(|index| raw.get(index))
                            .is_some_and(|byte| matches!(byte, b'f' | b'F'))));
            if *byte == b'\\'
                || (quote.is_none()
                    && (tail.starts_with(&[34, 34, 34]) || tail.starts_with(&[39, 39, 39])))
                || f_string
                || (*byte == b'\n' && quote.is_some())
            {
                return Ok(at);
            }
            if quoted {
                if quote == Some(*byte) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(*byte);
                }
            }
        }
    }
    Ok(raw.len())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ProofCapacities {
    definitions: usize,
    markers: usize,
    assignment_signatures: usize,
    iterable_signatures: usize,
    references: usize,
    variables: usize,
}

impl ProofCapacities {
    fn new(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut counts = Self::default();
        for fact in facts {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::Occurrence
                && (fact.syntax_kind().as_str().ends_with(".definition")
                    || fact.syntax_kind().as_str().ends_with(".definition_part"))
            {
                counts.definitions += 1;
            }
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Signature, "python.module_target.signature") => {
                    counts.markers += 1;
                }
                (SyntaxFactKind::Signature, "python.assignment_binding_unavailable.signature") => {
                    counts.assignment_signatures += 1;
                }
                (SyntaxFactKind::Signature, "python.module_iterable_context.signature") => {
                    counts.iterable_signatures += 1;
                }
                _ => {}
            }
            if required(fact) {
                counts.references += 1;
            }
        }
        // No certificate can be produced without a required read. Avoid all
        // proof tables and the entity census for this common sparse case.
        if counts.references == 0 {
            return Ok(counts);
        }
        for entity in entities.values() {
            cancel.check()?;
            if entity.record.language == "python" && entity.record.kind == EntityKind::Variable {
                counts.variables += 1;
            }
        }
        Ok(counts)
    }
}

struct Binding {
    target: SymbolId,
    owner: SymbolId,
    physical: SourceSpan,
    logical: SourceSpan,
    provenance: FactId,
    spelling: ContentHash,
}

pub(super) struct ModuleTargets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl ModuleTargets {
    /// Construct only complete raw/source/entity joins; ambiguous evidence refuses.
    /// The caller supplies decoded text and its exact physical coordinate mapping.
    /// Inconsistent evidence yields no bindings; cancellation and allocation remain errors.
    pub(super) fn new<'text>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut output = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        let capacities = ProofCapacities::new(facts, entities, cancel)?;
        if capacities.references == 0 {
            return Ok(output);
        }
        let mut ids = HashMap::new();
        let mut counts = HashMap::new();
        let mut definitions = HashMap::new();
        let mut markers = HashMap::new();
        let mut assignment_signatures = HashMap::new();
        let mut iterable_signatures = Vec::new();
        let (mut roots, mut modules, mut contexts) = (Vec::new(), Vec::new(), Vec::new());
        ids.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        definitions
            .try_reserve(capacities.definitions)
            .map_err(|_| SinkError::AllocationFailed)?;
        markers
            .try_reserve(capacities.markers)
            .map_err(|_| SinkError::AllocationFailed)?;
        assignment_signatures
            .try_reserve(capacities.assignment_signatures)
            .map_err(|_| SinkError::AllocationFailed)?;
        iterable_signatures
            .try_reserve(capacities.iterable_signatures)
            .map_err(|_| SinkError::AllocationFailed)?;
        counts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        roots
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        modules
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        contexts
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            if fact.span().file() != source.span().file()
                || ids.insert(fact.local_id(), fact).is_some()
            {
                return Ok(output);
            }
            counts
                .entry((fact.kind(), fact.span()))
                .and_modify(|count| *count = 2_usize)
                .or_insert(1);
            if fact.kind() == SyntaxFactKind::Root {
                if !roots.is_empty() {
                    return Ok(output);
                }
                roots.push(fact);
            }
            if fact.kind() == SyntaxFactKind::Module {
                if !modules.is_empty() {
                    return Ok(output);
                }
                modules.push(fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && (fact.syntax_kind().as_str().ends_with(".definition")
                    || fact.syntax_kind().as_str().ends_with(".definition_part"))
            {
                unique(&mut definitions, fact.parent().unwrap_or(u64::MAX), fact);
            }
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Signature, "python.module_binding_context.signature") => {
                    if !contexts.is_empty() {
                        return Ok(output);
                    }
                    contexts.push(fact);
                }
                (SyntaxFactKind::Signature, "python.module_target.signature") => {
                    unique(&mut markers, fact.span().end_byte(), fact)
                }
                (SyntaxFactKind::Signature, "python.module_iterable_context.signature") => {
                    iterable_signatures.push(fact);
                }
                (SyntaxFactKind::Signature, "python.assignment_binding_unavailable.signature") => {
                    unique(
                        &mut assignment_signatures,
                        fact.parent().unwrap_or(u64::MAX),
                        fact,
                    )
                }
                _ => {}
            }
        }
        let ([root], [module], [context]) =
            (roots.as_slice(), modules.as_slice(), contexts.as_slice())
        else {
            return Ok(output);
        };
        if source.span().start_byte() != 0
            || root.syntax_kind().as_str() != "python.module.root"
            || root.parent().is_some()
            || module.syntax_kind().as_str() != "python.file.module"
            || module.parent() != Some(root.local_id())
            || context.parent() != Some(module.local_id())
            || counts.get(&(context.kind(), context.span())) != Some(&1)
            || physical(root.span())? != source.span()
            || physical(module.span())? != source.span()
            || physical(context.span())? != source.span()
        {
            return Ok(output);
        }
        let Some(module_entity) = entities.get(&module.local_id()) else {
            return Ok(output);
        };
        if module_entity.record.kind != EntityKind::Module
            || module_entity.record.language != "python"
            || module_entity.record.repository != source.repository()
            || module_entity.record.generation != source.generation()
            || module_entity.record.container != Some(ContainerRef::File(source.span().file()))
            || module_entity.direct_parent != ContainerRef::File(source.span().file())
            || module_entity.definition_local_id.is_some()
            || module_entity
                .record
                .evidence
                .source
                .as_ref()
                .is_none_or(|evidence| {
                    !same_source(evidence, source) || evidence.span() != source.span()
                })
        {
            return Ok(output);
        }
        let mut variables = HashMap::new();
        let mut names = HashMap::new();
        let mut target_ends = HashMap::new();
        variables
            .try_reserve(capacities.variables)
            .map_err(|_| SinkError::AllocationFailed)?;
        names
            .try_reserve(capacities.variables)
            .map_err(|_| SinkError::AllocationFailed)?;
        target_ends
            .try_reserve(capacities.variables)
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            cancel.check()?;
            if entity.record.language != "python" || entity.record.kind != EntityKind::Variable {
                continue;
            }
            let Some(declaration) = ids.get(local).copied() else {
                return Ok(output);
            };
            let Some(definition) = definitions.get(local).copied().flatten() else {
                return Ok(output);
            };
            let Some(evidence) = entity.record.evidence.source.as_ref() else {
                return Ok(output);
            };
            let spelling = text(definition.span())?;
            let Some(key) = digest(spelling, cancel)? else {
                return Ok(output);
            };
            if declaration.kind() != SyntaxFactKind::Declaration
                || declaration.syntax_kind().as_str() != "python.module_variable.declaration"
                || declaration.parent() != Some(module.local_id())
                || counts.get(&(declaration.kind(), declaration.span())) != Some(&1)
                || definition.kind() != SyntaxFactKind::Occurrence
                || definition.syntax_kind().as_str() != "python.module_variable_name.definition"
                || definition.parent() != Some(*local)
                || counts.get(&(definition.kind(), definition.span())) != Some(&1)
                || entity.definition_local_id != Some(definition.local_id())
                || declaration.span().start_byte() != definition.span().start_byte()
                || !super::span_contains(declaration.span(), definition.span())
                || physical(declaration.span())? != evidence.span()
                || !same_source(evidence, source)
                || entity.record.repository != source.repository()
                || entity.record.generation != source.generation()
                || entity.record.provenance != module_entity.record.provenance
                || entity.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || entity.record.container != Some(entity.direct_parent)
                || !equal(spelling, &entity.record.canonical_name, cancel)?
            {
                return Ok(output);
            }
            unique(&mut names, key, *local);
            unique(
                &mut variables,
                declaration.span().start_byte(),
                (declaration, definition, entity, key),
            );
            unique(
                &mut target_ends,
                definition.span().end_byte(),
                declaration.span().start_byte(),
            );
        }
        let mut iterable_positions = HashMap::new();
        iterable_positions
            .try_reserve(capacities.references)
            .map_err(|_| SinkError::AllocationFailed)?;
        if !iterable_signatures.is_empty() {
            let module_text = text(root.span())?;
            let prefix_limit = iterable_prefix_limit(module_text, cancel)?;
            for header in iterable_signatures {
                cancel.check()?;
                if header.parent() != Some(module.local_id())
                    || counts.get(&(header.kind(), header.span())) != Some(&1)
                    || !super::span_contains(root.span(), header.span())
                    || !super::span_contains(source.span(), physical(header.span())?)
                {
                    continue;
                }
                let Some(start) = header
                    .span()
                    .start_byte()
                    .checked_sub(root.span().start_byte())
                    .and_then(|offset| usize::try_from(offset).ok())
                else {
                    continue;
                };
                let Some(end) = header
                    .span()
                    .end_byte()
                    .checked_sub(root.span().start_byte())
                    .and_then(|offset| usize::try_from(offset).ok())
                else {
                    continue;
                };
                if start > prefix_limit
                    || (start > 0 && module_text.as_bytes().get(start - 1) != Some(&b'\n'))
                {
                    continue;
                }
                let header_text = text(header.span())?;
                if module_text
                    .get(start..end)
                    .is_none_or(|current| current != header_text)
                {
                    continue;
                }
                let Some(words) = iterable_names(header_text, cancel)? else {
                    continue;
                };
                for (start, end) in words {
                    cancel.check()?;
                    let Some(start) = u64::try_from(start)
                        .ok()
                        .and_then(|offset| header.span().start_byte().checked_add(offset))
                    else {
                        return Err(super::provider_failure("python-module-coordinate-overflow"));
                    };
                    let Some(end) = u64::try_from(end)
                        .ok()
                        .and_then(|offset| header.span().start_byte().checked_add(offset))
                    else {
                        return Err(super::provider_failure("python-module-coordinate-overflow"));
                    };
                    iterable_positions
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    unique(&mut iterable_positions, (start, end), header);
                }
            }
        }
        let mut flat = HashMap::new();
        flat.try_reserve(variables.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        output
            .bindings
            .try_reserve(capacities.references)
            .map_err(|_| SinkError::AllocationFailed)?;
        for reference in facts.iter().filter(|fact| required(fact)) {
            cancel.check()?;
            let Some(owner_id) = reference.parent() else {
                continue;
            };
            let (owner_span, owner) = if owner_id == module.local_id() {
                let Some(header) = iterable_positions
                    .get(&(reference.span().start_byte(), reference.span().end_byte()))
                    .copied()
                    .flatten()
                else {
                    continue;
                };
                if counts.get(&(reference.kind(), reference.span())) != Some(&1) {
                    continue;
                }
                (header.span(), module_entity)
            } else {
                let Some(owner_decl) = ids.get(&owner_id).copied() else {
                    continue;
                };
                let Some((owner_decl, owner_def, owner, owner_key)) = variables
                    .get(&owner_decl.span().start_byte())
                    .copied()
                    .flatten()
                else {
                    continue;
                };
                if owner_decl.local_id() != owner_id
                    || names.get(&owner_key) != Some(&Some(owner_id))
                {
                    continue;
                }
                let Some(signature) = assignment_signatures.get(&owner_id).copied().flatten()
                else {
                    continue;
                };
                if signature.span() != owner_decl.span()
                    || counts.get(&(signature.kind(), signature.span())) != Some(&1)
                    || counts.get(&(reference.kind(), reference.span())) != Some(&1)
                {
                    continue;
                }
                if let std::collections::hash_map::Entry::Vacant(entry) = flat.entry(owner_id) {
                    let words = flat_names(
                        text(owner_decl.span())?,
                        text(owner_def.span())?.len(),
                        cancel,
                    )?;
                    let mut positions = HashSet::new();
                    if let Some(words) = &words {
                        positions
                            .try_reserve(words.len())
                            .map_err(|_| SinkError::AllocationFailed)?;
                        for (start, end) in words {
                            cancel.check()?;
                            let Some(start) = u64::try_from(*start).ok().and_then(|offset| {
                                owner_decl.span().start_byte().checked_add(offset)
                            }) else {
                                return Err(super::provider_failure(
                                    "python-module-coordinate-overflow",
                                ));
                            };
                            let Some(end) = u64::try_from(*end).ok().and_then(|offset| {
                                owner_decl.span().start_byte().checked_add(offset)
                            }) else {
                                return Err(super::provider_failure(
                                    "python-module-coordinate-overflow",
                                ));
                            };
                            positions.insert((start, end));
                        }
                    }
                    entry.insert(words.map(|_| positions));
                }
                if !flat
                    .get(&owner_id)
                    .and_then(Option::as_ref)
                    .is_some_and(|positions| {
                        positions
                            .contains(&(reference.span().start_byte(), reference.span().end_byte()))
                    })
                {
                    continue;
                }
                (owner_decl.span(), owner)
            };
            let Some(marker) = markers.get(&reference.span().end_byte()).copied().flatten() else {
                continue;
            };
            if marker.parent() != Some(module.local_id())
                || counts.get(&(marker.kind(), marker.span())) != Some(&1)
            {
                continue;
            }
            let Some(target_start) = target_ends
                .get(&marker.span().start_byte())
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some((target_decl, target_def, target, key)) =
                variables.get(&target_start).copied().flatten()
            else {
                continue;
            };
            if names.get(&key) != Some(&Some(target_decl.local_id()))
                || target_decl.span().end_byte() > owner_span.start_byte()
                || marker.span().end_byte() != reference.span().end_byte()
                || !super::span_contains(source.span(), physical(marker.span())?)
                || !super::span_contains(owner_span, reference.span())
                || !equal(text(target_def.span())?, text(reference.span())?, cancel)?
            {
                continue;
            }
            let Some(spelling) = digest(text(reference.span())?, cancel)? else {
                continue;
            };
            let physical_reference = physical(reference.span())?;
            if owner
                .record
                .evidence
                .source
                .as_ref()
                .is_none_or(|evidence| !super::span_contains(evidence.span(), physical_reference))
            {
                continue;
            }
            output.bindings.insert(
                reference.local_id(),
                Binding {
                    target: target.record.id,
                    owner: owner.record.id,
                    physical: physical_reference,
                    logical: reference.span(),
                    provenance: target.record.provenance,
                    spelling,
                },
            );
        }
        Ok(output)
    }

    /// Adopt only the exact current unresolved occurrence proven during construction.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        Ok((required(fact) && fact.span() == binding.logical && physical == binding.physical && occurrence.source.span() == physical
            && occurrence.role == OccurrenceRole::Reference && occurrence.syntax_kind == "python.module_read.reference"
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { text_hash } if text_hash == binding.spelling) && occurrence.enclosing == Some(binding.owner)
            && occurrence.repository == self.source.repository() && occurrence.generation == self.source.generation()
            && occurrence.file == self.source.span().file() && same_source(&occurrence.source, &self.source)
            && occurrence.provenance == binding.provenance && occurrence.syntactic_text_hash == binding.spelling
            && occurrence.evidence.source.as_ref() == Some(&occurrence.source)).then_some(binding.target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_ids::{FileId, GenerationId, RepositoryId, content_hash};
    use rootlight_ir::{
        AnalysisTier, Confidence, EntityRecord, EntityVisibility, FactEvidence, SymbolIdentityClaim,
    };

    const TEXT: &str = "value = 1\nvalue += 2\ncopy = value / 2\nshadow = lambda value: value\n";

    fn span(start: usize, end: usize) -> SourceSpan {
        SourceSpan::new(
            FileId::from_bytes([3; 20]),
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        )
        .unwrap()
    }

    fn source(at: SourceSpan) -> SourceRef {
        source_for_text(at, TEXT)
    }

    fn source_for_text(at: SourceSpan, text: &str) -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            at,
            content_hash(text.as_bytes()),
            None,
        )
    }

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        start: usize,
        end: usize,
        label: &str,
    ) -> SyntaxFact {
        let depth = match parent {
            None => 0,
            Some(1) => 1,
            Some(2) => 2,
            Some(_) => 3,
        };
        SyntaxFact::new(
            id,
            parent,
            kind,
            span(start, end),
            depth,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn raw() -> Vec<SyntaxFact> {
        let copy = TEXT.find("copy =").unwrap();
        let copy_end = TEXT[copy..].find('\n').unwrap() + copy;
        let shadow = TEXT.find("shadow =").unwrap();
        let shadow_end = TEXT.len() - 1;
        let read = TEXT.find("value / 2").unwrap();
        vec![
            fact(
                1,
                None,
                SyntaxFactKind::Root,
                0,
                TEXT.len(),
                "python.module.root",
            ),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Module,
                0,
                TEXT.len(),
                "python.file.module",
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Signature,
                0,
                TEXT.len(),
                "python.module_binding_context.signature",
            ),
            fact(
                4,
                Some(2),
                SyntaxFactKind::Declaration,
                0,
                9,
                "python.module_variable.declaration",
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Occurrence,
                0,
                5,
                "python.module_variable_name.definition",
            ),
            fact(
                6,
                Some(4),
                SyntaxFactKind::Signature,
                0,
                9,
                "python.assignment_binding_unavailable.signature",
            ),
            fact(
                7,
                Some(2),
                SyntaxFactKind::Declaration,
                copy,
                copy_end,
                "python.module_variable.declaration",
            ),
            fact(
                8,
                Some(7),
                SyntaxFactKind::Occurrence,
                copy,
                copy + 4,
                "python.module_variable_name.definition",
            ),
            fact(
                9,
                Some(7),
                SyntaxFactKind::Signature,
                copy,
                copy_end,
                "python.assignment_binding_unavailable.signature",
            ),
            fact(
                10,
                Some(7),
                SyntaxFactKind::Occurrence,
                read,
                read + 5,
                "python.module_read.reference",
            ),
            fact(
                11,
                Some(2),
                SyntaxFactKind::Signature,
                5,
                read + 5,
                "python.module_target.signature",
            ),
            fact(
                12,
                Some(2),
                SyntaxFactKind::Declaration,
                shadow,
                shadow_end,
                "python.module_variable.declaration",
            ),
            fact(
                13,
                Some(12),
                SyntaxFactKind::Occurrence,
                shadow,
                shadow + 6,
                "python.module_variable_name.definition",
            ),
            fact(
                14,
                Some(12),
                SyntaxFactKind::Signature,
                shadow,
                shadow_end,
                "python.assignment_binding_unavailable.signature",
            ),
        ]
    }

    fn entity(
        local: u64,
        kind: EntityKind,
        name: &str,
        at: SourceSpan,
        definition: Option<u64>,
        parent: ContainerRef,
    ) -> MaterializedEntity {
        let full = source(span(0, TEXT.len()));
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: full.repository(),
            language: "python".to_owned(),
            kind,
            container: Some(parent),
            container_identity: vec![u8::try_from(local).unwrap()],
            declared_identity: name.to_owned(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: Vec::new(),
        };
        claim.symbol = claim.derived_symbol();
        MaterializedEntity {
            record: EntityRecord {
                id: claim.symbol,
                repository: full.repository(),
                generation: full.generation(),
                kind,
                language: "python".to_owned(),
                tier: AnalysisTier::TierD,
                canonical_name: name.to_owned(),
                display_name: name.to_owned(),
                qualified_name: name.to_owned(),
                container: Some(parent),
                visibility: EntityVisibility::Unknown,
                flags: Vec::new(),
                provenance: FactId::from_bytes([4; 20]),
                evidence: FactEvidence {
                    source: Some(source(at)),
                    derivation: Vec::new(),
                },
            },
            identity_claim: claim,
            direct_parent: parent,
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
            "written.py",
            span(0, TEXT.len()),
            None,
            ContainerRef::File(span(0, 0).file()),
        );
        let parent = ContainerRef::Entity(module.record.id);
        let copy = TEXT.find("copy =").unwrap();
        let copy_end = TEXT[copy..].find('\n').unwrap() + copy;
        let shadow = TEXT.find("shadow =").unwrap();
        HashMap::from([
            (2, module),
            (
                4,
                entity(
                    4,
                    EntityKind::Variable,
                    "value",
                    span(0, 9),
                    Some(5),
                    parent,
                ),
            ),
            (
                7,
                entity(
                    7,
                    EntityKind::Variable,
                    "copy",
                    span(copy, copy_end),
                    Some(8),
                    parent,
                ),
            ),
            (
                12,
                entity(
                    12,
                    EntityKind::Variable,
                    "shadow",
                    span(shadow, TEXT.len() - 1),
                    Some(13),
                    parent,
                ),
            ),
        ])
    }

    fn site(fact: &SyntaxFact, owner: SymbolId) -> OccurrenceRecord {
        site_for_text(fact, owner, TEXT)
    }

    fn site_for_text(fact: &SyntaxFact, owner: SymbolId, text: &str) -> OccurrenceRecord {
        let current = source_for_text(fact.span(), text);
        let start = usize::try_from(fact.span().start_byte()).unwrap();
        let end = usize::try_from(fact.span().end_byte()).unwrap();
        let hash = content_hash(&text.as_bytes()[start..end]);
        OccurrenceRecord {
            id: FactId::from_bytes([8; 20]),
            repository: current.repository(),
            generation: current.generation(),
            file: current.span().file(),
            source: current.clone(),
            role: OccurrenceRole::Reference,
            enclosing: Some(owner),
            target: OccurrenceTarget::Unresolved { text_hash: hash },
            syntactic_text_hash: hash,
            syntax_kind: "python.module_read.reference".to_owned(),
            provenance: FactId::from_bytes([4; 20]),
            confidence: Confidence::new(850).unwrap(),
            evidence: FactEvidence {
                source: Some(current),
                derivation: Vec::new(),
            },
        }
    }

    fn targets(facts: &[SyntaxFact], entities: &HashMap<u64, MaterializedEntity>) -> ModuleTargets {
        ModuleTargets::new(
            facts,
            entities,
            &source(span(0, TEXT.len())),
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
    fn current_flat_rhs_rejects_lambda_calls_members_subscripts_comments_and_partial_tokens() {
        for valid in [
            "copy=value / 2",
            "copy = (value + 2) * -3.0",
            "copy = value << 1",
            "copy = value / 2e+1",
        ] {
            let names = flat_names(valid, 4, &Cancellation::new()).unwrap().unwrap();
            assert_eq!(names.len(), 1);
            assert_eq!(&valid[names[0].0..names[0].1], "value");
        }
        for invalid in [
            "copy=lambda value:value",
            "copy = value()",
            "copy = holder.value",
            "copy = value[0]",
            "copy = [value for value in values]",
            "copy = value # context",
            "copy = (value := 1)",
            "copy == value",
            "copy = value if ready else other",
            "copy = 'value'",
            "copy = value\n",
            "copy = value +",
            "copy = (value",
        ] {
            assert!(
                flat_names(invalid, 4, &Cancellation::new())
                    .unwrap()
                    .is_none(),
                "{invalid}"
            );
        }
        let containing = "copy = myvalue / 2";
        let words = flat_names(containing, 4, &Cancellation::new())
            .unwrap()
            .unwrap();
        assert_eq!(&containing[words[0].0..words[0].1], "myvalue");
        assert!(
            !words.contains(&(9, 14)),
            "a suffix spelling cannot replace a complete identifier token"
        );
    }

    #[test]
    fn current_graph_cardinality_parent_chains_and_rhs_context_are_independent_requirements() {
        let original = raw();
        let records = entities();
        let reference = &original[9];
        let occurrence = site(reference, records[&7].record.id);
        assert_eq!(
            targets(&original, &records)
                .resolve(
                    reference,
                    reference.span(),
                    &occurrence,
                    &Cancellation::new()
                )
                .unwrap(),
            Some(records[&4].record.id)
        );
        for mutation in 0..15 {
            let mut facts = original.clone();
            match mutation {
                0 => {
                    facts.remove(2);
                }
                1 => facts.push(facts[9].clone()),
                2 => facts.push(fact(
                    99,
                    Some(2),
                    SyntaxFactKind::Declaration,
                    0,
                    9,
                    "python.foreign.declaration",
                )),
                3 => facts.push(fact(
                    99,
                    Some(4),
                    SyntaxFactKind::Occurrence,
                    0,
                    5,
                    "python.foreign.definition",
                )),
                4 => facts.push(fact(
                    99,
                    Some(2),
                    SyntaxFactKind::Signature,
                    5,
                    usize::try_from(reference.span().end_byte()).unwrap(),
                    "python.foreign.signature",
                )),
                5 => {
                    facts[9] = fact(
                        10,
                        Some(2),
                        SyntaxFactKind::Occurrence,
                        usize::try_from(reference.span().start_byte()).unwrap(),
                        usize::try_from(reference.span().end_byte()).unwrap(),
                        "python.module_read.reference",
                    )
                }
                6 => {
                    facts[10] = fact(
                        11,
                        Some(1),
                        SyntaxFactKind::Signature,
                        5,
                        usize::try_from(reference.span().end_byte()).unwrap(),
                        "python.module_target.signature",
                    )
                }
                7 => {
                    facts[8] = fact(
                        9,
                        Some(2),
                        SyntaxFactKind::Signature,
                        usize::try_from(facts[8].span().start_byte()).unwrap(),
                        usize::try_from(facts[8].span().end_byte()).unwrap(),
                        "python.assignment_binding_unavailable.signature",
                    )
                }
                8 => {
                    facts[1] = fact(
                        2,
                        None,
                        SyntaxFactKind::Module,
                        0,
                        TEXT.len(),
                        "python.file.module",
                    )
                }
                9 => {
                    facts[10] = fact(
                        11,
                        Some(2),
                        SyntaxFactKind::Signature,
                        6,
                        usize::try_from(reference.span().end_byte()).unwrap(),
                        "python.module_target.signature",
                    )
                }
                10 => facts.push(fact(
                    99,
                    Some(7),
                    SyntaxFactKind::Occurrence,
                    usize::try_from(reference.span().start_byte()).unwrap(),
                    usize::try_from(reference.span().end_byte()).unwrap(),
                    "python.foreign.reference",
                )),
                11 => {
                    facts[4] = fact(
                        5,
                        Some(2),
                        SyntaxFactKind::Occurrence,
                        0,
                        5,
                        "python.module_variable_name.definition",
                    )
                }
                12 => {
                    facts.remove(8);
                }
                13 => facts.push(fact(
                    99,
                    Some(2),
                    SyntaxFactKind::Module,
                    0,
                    TEXT.len(),
                    "python.foreign.module",
                )),
                14 => {
                    let start = TEXT.rfind("value").unwrap();
                    facts[9] = fact(
                        10,
                        Some(12),
                        SyntaxFactKind::Occurrence,
                        start,
                        start + 5,
                        "python.module_read.reference",
                    );
                    facts[10] = fact(
                        11,
                        Some(2),
                        SyntaxFactKind::Signature,
                        5,
                        start + 5,
                        "python.module_target.signature",
                    );
                }
                _ => unreachable!(),
            }
            let candidate = facts.iter().find(|fact| fact.local_id() == 10).unwrap();
            let owner = if mutation == 14 {
                records[&12].record.id
            } else {
                records[&7].record.id
            };
            let occurrence = site(candidate, owner);
            assert_eq!(
                targets(&facts, &records)
                    .resolve(
                        candidate,
                        candidate.span(),
                        &occurrence,
                        &Cancellation::new()
                    )
                    .unwrap(),
                None,
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn current_binding_refuses_entity_generation_source_provenance_and_occurrence_tampering() {
        let facts = raw();
        let original = entities();
        let reference = &facts[9];
        for mutation in 0..6 {
            let mut records = original.clone();
            let target = records.get_mut(&4).unwrap();
            match mutation {
                0 => target.record.generation = GenerationId::from_bytes([9; 20]),
                1 => target.record.provenance = FactId::from_bytes([9; 20]),
                2 => target.record.canonical_name = "other".to_owned(),
                3 => target.definition_local_id = Some(8),
                4 => {
                    target.record.evidence.source = Some(SourceRef::new(
                        source(span(0, 9)).repository(),
                        source(span(0, 9)).generation(),
                        span(0, 9),
                        content_hash(b"changed"),
                        None,
                    ))
                }
                5 => target.direct_parent = ContainerRef::File(span(0, 0).file()),
                _ => unreachable!(),
            }
            let occurrence = site(reference, original[&7].record.id);
            assert_eq!(
                targets(&facts, &records)
                    .resolve(
                        reference,
                        reference.span(),
                        &occurrence,
                        &Cancellation::new()
                    )
                    .unwrap(),
                None,
                "entity mutation {mutation}"
            );
        }
        let proof = targets(&facts, &original);
        for mutation in 0..6 {
            let mut occurrence = site(reference, original[&7].record.id);
            match mutation {
                0 => occurrence.generation = GenerationId::from_bytes([9; 20]),
                1 => occurrence.provenance = FactId::from_bytes([9; 20]),
                2 => occurrence.enclosing = Some(original[&4].record.id),
                3 => occurrence.syntactic_text_hash = content_hash(b"other"),
                4 => occurrence.evidence.source = None,
                5 => {
                    occurrence.target = OccurrenceTarget::Unresolved {
                        text_hash: content_hash(b"other"),
                    }
                }
                _ => unreachable!(),
            }
            assert_eq!(
                proof
                    .resolve(
                        reference,
                        reference.span(),
                        &occurrence,
                        &Cancellation::new()
                    )
                    .unwrap(),
                None
            );
        }
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            proof.resolve(
                reference,
                reference.span(),
                &site(reference, original[&7].record.id),
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn sparse_census_counts_foreign_definitions_and_closed_roles_only() {
        let mut facts = raw();
        facts.extend([
            fact(
                90,
                Some(4),
                SyntaxFactKind::Occurrence,
                0,
                5,
                "foreign.definition",
            ),
            fact(
                91,
                Some(4),
                SyntaxFactKind::Occurrence,
                0,
                5,
                "foreign.definition_part",
            ),
            fact(
                92,
                Some(4),
                SyntaxFactKind::Signature,
                0,
                5,
                "foreign.definition",
            ),
            fact(
                93,
                Some(2),
                SyntaxFactKind::Occurrence,
                0,
                5,
                "python.module_target.signature",
            ),
            fact(
                94,
                Some(2),
                SyntaxFactKind::Signature,
                0,
                5,
                "python.module_target.signature.extra",
            ),
            fact(
                95,
                Some(4),
                SyntaxFactKind::Signature,
                0,
                5,
                "python.assignment_binding_unavailable.signature.extra",
            ),
            fact(
                96,
                Some(7),
                SyntaxFactKind::Signature,
                0,
                5,
                "python.module_read.reference",
            ),
        ]);
        for id in 100..228 {
            facts.push(fact(
                id,
                Some(2),
                SyntaxFactKind::Signature,
                0,
                5,
                "python.unrelated.signature",
            ));
        }
        let mut records = entities();
        let mut foreign = records[&4].clone();
        foreign.record.language = "ruby".to_owned();
        records.insert(90, foreign);
        let mut function = records[&4].clone();
        function.record.kind = EntityKind::Function;
        records.insert(91, function);
        assert_eq!(
            ProofCapacities::new(&facts, &records, &Cancellation::new()).unwrap(),
            ProofCapacities {
                definitions: 5,
                markers: 1,
                assignment_signatures: 3,
                iterable_signatures: 0,
                references: 1,
                variables: 3,
            }
        );
    }

    #[test]
    fn absent_required_reads_leave_zero_capacity_and_never_request_source_certificates() {
        let records = entities();
        let mut unrelated = raw();
        unrelated.retain(|fact| !required(fact));
        for facts in [Vec::new(), unrelated] {
            let capacities = ProofCapacities::new(&facts, &records, &Cancellation::new()).unwrap();
            assert_eq!(capacities.references, 0);
            assert_eq!(capacities.variables, 0);
            let proof = ModuleTargets::new(
                &facts,
                &records,
                &source(span(0, TEXT.len())),
                |_| panic!("no required read may request source text"),
                |_| panic!("no required read may request a physical certificate"),
                &Cancellation::new(),
            )
            .unwrap();
            assert!(proof.bindings.is_empty());
            assert_eq!(proof.bindings.capacity(), 0);
        }
    }

    #[test]
    fn cancelled_census_and_empty_read_path_do_not_return_a_successful_proof() {
        let records = entities();
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        for facts in [Vec::new(), raw()] {
            assert!(matches!(
                ProofCapacities::new(&facts, &records, &cancelled),
                Err(AdapterError::Cancelled { .. })
            ));
            assert!(matches!(
                ModuleTargets::new(
                    &facts,
                    &records,
                    &source(span(0, TEXT.len())),
                    |_| panic!("cancelled proof must not request source text"),
                    |_| panic!("cancelled proof must not request physical coordinates"),
                    &cancelled,
                ),
                Err(AdapterError::Cancelled { .. })
            ));
        }
    }

    const ITERABLE_TEXT: &str =
        "LIMIT = 4\nOFFSET = 2\nfor slot in producer(LIMIT, (OFFSET + 1)):\n    pass\n";

    fn iterable_fixture(text: &str) -> (Vec<SyntaxFact>, HashMap<u64, MaterializedEntity>) {
        let limit = text.find("LIMIT =").unwrap();
        let limit_end = text[limit..].find('\n').unwrap() + limit;
        let offset = text.find("OFFSET =").unwrap();
        let offset_end = text[offset..].find('\n').unwrap() + offset;
        let header = text.find("for slot in").unwrap();
        let header_end = text[header..].find(':').unwrap() + header + 1;
        let limit_read = text.rfind("LIMIT").unwrap();
        let offset_read = text.rfind("OFFSET").unwrap();
        let facts = vec![
            fact(
                1,
                None,
                SyntaxFactKind::Root,
                0,
                text.len(),
                "python.module.root",
            ),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Module,
                0,
                text.len(),
                "python.file.module",
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Signature,
                0,
                text.len(),
                "python.module_binding_context.signature",
            ),
            fact(
                4,
                Some(2),
                SyntaxFactKind::Declaration,
                limit,
                limit_end,
                "python.module_variable.declaration",
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Occurrence,
                limit,
                limit + 5,
                "python.module_variable_name.definition",
            ),
            fact(
                6,
                Some(4),
                SyntaxFactKind::Signature,
                limit,
                limit_end,
                "python.assignment_binding_unavailable.signature",
            ),
            fact(
                7,
                Some(2),
                SyntaxFactKind::Declaration,
                offset,
                offset_end,
                "python.module_variable.declaration",
            ),
            fact(
                8,
                Some(7),
                SyntaxFactKind::Occurrence,
                offset,
                offset + 6,
                "python.module_variable_name.definition",
            ),
            fact(
                9,
                Some(7),
                SyntaxFactKind::Signature,
                offset,
                offset_end,
                "python.assignment_binding_unavailable.signature",
            ),
            fact(
                10,
                Some(2),
                SyntaxFactKind::Signature,
                header,
                header_end,
                "python.module_iterable_context.signature",
            ),
            fact(
                11,
                Some(2),
                SyntaxFactKind::Occurrence,
                limit_read,
                limit_read + 5,
                "python.module_read.reference",
            ),
            fact(
                12,
                Some(2),
                SyntaxFactKind::Signature,
                limit + 5,
                limit_read + 5,
                "python.module_target.signature",
            ),
            fact(
                13,
                Some(2),
                SyntaxFactKind::Occurrence,
                offset_read,
                offset_read + 6,
                "python.module_read.reference",
            ),
            fact(
                14,
                Some(2),
                SyntaxFactKind::Signature,
                offset + 6,
                offset_read + 6,
                "python.module_target.signature",
            ),
        ];
        let module = entity(
            2,
            EntityKind::Module,
            "written.py",
            span(0, text.len()),
            None,
            ContainerRef::File(span(0, 0).file()),
        );
        let parent = ContainerRef::Entity(module.record.id);
        let mut records = HashMap::from([
            (2, module),
            (
                4,
                entity(
                    4,
                    EntityKind::Variable,
                    "LIMIT",
                    span(limit, limit_end),
                    Some(5),
                    parent,
                ),
            ),
            (
                7,
                entity(
                    7,
                    EntityKind::Variable,
                    "OFFSET",
                    span(offset, offset_end),
                    Some(8),
                    parent,
                ),
            ),
        ]);
        for record in records.values_mut() {
            let at = record.record.evidence.source.as_ref().unwrap().span();
            record.record.evidence.source = Some(source_for_text(at, text));
        }
        (facts, records)
    }

    fn iterable_targets(
        text: &str,
        facts: &[SyntaxFact],
        records: &HashMap<u64, MaterializedEntity>,
    ) -> ModuleTargets {
        ModuleTargets::new(
            facts,
            records,
            &source_for_text(span(0, text.len()), text),
            |at| {
                Ok(&text[usize::try_from(at.start_byte()).unwrap()
                    ..usize::try_from(at.end_byte()).unwrap()])
            },
            Ok,
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn current_iterable_lexer_proves_only_positional_operand_ranges() {
        for header in [
            "for slot in producer(LIMIT, (OFFSET + 1)):",
            "for renamed in arbitrary(LIMIT,):",
            "for n in f (LIMIT / 2e+1):",
        ] {
            let names = iterable_names(header, &Cancellation::new())
                .unwrap()
                .unwrap();
            assert!(!names.is_empty());
            assert!(
                names
                    .iter()
                    .all(|(start, end)| matches!(&header[*start..*end], "LIMIT" | "OFFSET"))
            );
        }
        for header in [
            "for slot in producer(stop=LIMIT):",
            "for slot in producer(*LIMIT):",
            "for slot in owner.producer(LIMIT):",
            "for slot in producer(LIMIT[0]):",
            "for slot in producer(nested(LIMIT)):",
            "for slot in producer(lambda: LIMIT):",
            "for slot in producer((LIMIT := 1)):",
            "for slot in producer([LIMIT for n in values]):",
            "for slot in producer(LIMIT) + other:",
            "for slot in producer(LIMIT): pass",
            "for slot in producer(LIMIT):#comment",
            "for slot in producer(LIMIT\n):",
            "for slot in producer(,LIMIT):",
            "for slot in producer(LIMIT,,OFFSET):",
            "async for slot in producer(LIMIT):",
        ] {
            assert!(
                iterable_names(header, &Cancellation::new())
                    .unwrap()
                    .is_none(),
                "{header}"
            );
        }
    }

    #[test]
    fn iterable_context_requires_current_header_cardinality_and_exact_operand_positions() {
        let (original, records) = iterable_fixture(ITERABLE_TEXT);
        for index in [10, 12] {
            let reference = &original[index];
            let expected = if index == 10 {
                records[&4].record.id
            } else {
                records[&7].record.id
            };
            let occurrence = site_for_text(reference, records[&2].record.id, ITERABLE_TEXT);
            assert_eq!(
                iterable_targets(ITERABLE_TEXT, &original, &records)
                    .resolve(
                        reference,
                        reference.span(),
                        &occurrence,
                        &Cancellation::new()
                    )
                    .unwrap(),
                Some(expected)
            );
        }
        for mutation in 0..10 {
            let mut facts = original.clone();
            let reference = &original[10];
            let start = usize::try_from(reference.span().start_byte()).unwrap();
            let end = usize::try_from(reference.span().end_byte()).unwrap();
            let header_start = ITERABLE_TEXT.find("for slot").unwrap();
            let header_end = ITERABLE_TEXT.find(':').unwrap() + 1;
            match mutation {
                0 => {
                    facts.remove(9);
                }
                1 => facts.push(fact(
                    99,
                    Some(2),
                    SyntaxFactKind::Signature,
                    header_start,
                    header_end,
                    "python.module_iterable_context.signature",
                )),
                2 => {
                    facts.remove(11);
                }
                3 => facts.push(fact(
                    99,
                    Some(2),
                    SyntaxFactKind::Signature,
                    5,
                    end,
                    "python.module_target.signature",
                )),
                4 => {
                    facts[9] = fact(
                        10,
                        Some(4),
                        SyntaxFactKind::Signature,
                        header_start,
                        header_end,
                        "python.module_iterable_context.signature",
                    )
                }
                5 => {
                    facts[10] = fact(
                        11,
                        Some(4),
                        SyntaxFactKind::Occurrence,
                        start,
                        end,
                        "python.module_read.reference",
                    )
                }
                6 => {
                    facts[11] = fact(
                        12,
                        Some(2),
                        SyntaxFactKind::Signature,
                        4,
                        end,
                        "python.module_target.signature",
                    )
                }
                7 => {
                    facts[9] = fact(
                        10,
                        Some(2),
                        SyntaxFactKind::Signature,
                        header_start,
                        header_end + 1,
                        "python.module_iterable_context.signature",
                    )
                }
                8 => {
                    facts[9] = fact(
                        10,
                        Some(2),
                        SyntaxFactKind::Signature,
                        header_start + 4,
                        header_end,
                        "python.module_iterable_context.signature",
                    )
                }
                9 => facts.push(fact(
                    99,
                    Some(2),
                    SyntaxFactKind::Occurrence,
                    start,
                    end,
                    "python.foreign.reference",
                )),
                _ => unreachable!(),
            }
            let occurrence = site_for_text(reference, records[&2].record.id, ITERABLE_TEXT);
            assert_eq!(
                iterable_targets(ITERABLE_TEXT, &facts, &records)
                    .resolve(
                        reference,
                        reference.span(),
                        &occurrence,
                        &Cancellation::new()
                    )
                    .unwrap(),
                None,
                "{mutation}"
            );
        }
        for name in ["slot", "producer"] {
            let mut facts = original.clone();
            let start = ITERABLE_TEXT.find(name).unwrap();
            facts[10] = fact(
                11,
                Some(2),
                SyntaxFactKind::Occurrence,
                start,
                start + name.len(),
                "python.module_read.reference",
            );
            facts[11] = fact(
                12,
                Some(2),
                SyntaxFactKind::Signature,
                5,
                start + name.len(),
                "python.module_target.signature",
            );
            let reference = &facts[10];
            let occurrence = site_for_text(reference, records[&2].record.id, ITERABLE_TEXT);
            assert_eq!(
                iterable_targets(ITERABLE_TEXT, &facts, &records)
                    .resolve(
                        reference,
                        reference.span(),
                        &occurrence,
                        &Cancellation::new()
                    )
                    .unwrap(),
                None
            );
        }
    }

    #[test]
    fn relocated_headers_and_changed_current_arguments_never_inherit_native_authority() {
        let header = "for slot in producer(LIMIT, (OFFSET + 1)):";
        let prefix = "LIMIT = 4\nOFFSET = 2\n";
        let texts = [
            format!("{prefix}note = \"\"\"\n{header}\n\"\"\"\n"),
            format!("{prefix}# {header}\n"),
            format!("{prefix}note = \"{header}\"\n"),
            format!("{prefix}    {header}\n        pass\n"),
            format!("{prefix}note = f\"{{1}}\"\n{header}\n    pass\n"),
            format!("{prefix}for slot in producer(LIMIT, nested(OFFSET)):\n    pass\n"),
        ];
        for text in texts {
            let (facts, records) = iterable_fixture(&text);
            let reference = &facts[10];
            let occurrence = site_for_text(reference, records[&2].record.id, &text);
            assert_eq!(
                iterable_targets(&text, &facts, &records)
                    .resolve(
                        reference,
                        reference.span(),
                        &occurrence,
                        &Cancellation::new()
                    )
                    .unwrap(),
                None,
                "{text}"
            );
        }
        for prefix in [
            "note = 'leaf'\n",
            "note = \"surf\"\n",
            "# ordinary quotes 'leaf' and \"surf\"\n",
        ] {
            let text = format!("{prefix}{ITERABLE_TEXT}");
            let (facts, records) = iterable_fixture(&text);
            let reference = &facts[10];
            let occurrence = site_for_text(reference, records[&2].record.id, &text);
            assert_eq!(
                iterable_targets(&text, &facts, &records)
                    .resolve(
                        reference,
                        reference.span(),
                        &occurrence,
                        &Cancellation::new()
                    )
                    .unwrap(),
                Some(records[&4].record.id)
            );
        }
    }

    #[test]
    fn iterable_module_joins_preserve_physical_coordinates_source_and_owner_guards() {
        let text = format!("note = 'Ω'\n{ITERABLE_TEXT}");
        let (facts, mut records) = iterable_fixture(&text);
        let encoded: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let map = |logical: SourceSpan| {
            if logical == span(0, text.len()) {
                return Ok(span(0, encoded.len()));
            }
            let begin = usize::try_from(logical.start_byte()).unwrap();
            let end = usize::try_from(logical.end_byte()).unwrap();
            Ok(span(
                2 + text[..begin].encode_utf16().count() * 2,
                2 + text[..end].encode_utf16().count() * 2,
            ))
        };
        let full = SourceRef::new(
            source(span(0, 0)).repository(),
            source(span(0, 0)).generation(),
            span(0, encoded.len()),
            content_hash(&encoded),
            None,
        );
        for record in records.values_mut() {
            let at = map(record.record.evidence.source.as_ref().unwrap().span()).unwrap();
            record.record.evidence.source = Some(super::super::source_for_span(&full, at));
        }
        let proof = ModuleTargets::new(
            &facts,
            &records,
            &full,
            |at| {
                Ok(&text[usize::try_from(at.start_byte()).unwrap()
                    ..usize::try_from(at.end_byte()).unwrap()])
            },
            map,
            &Cancellation::new(),
        )
        .unwrap();
        let reference = &facts[10];
        let physical = map(reference.span()).unwrap();
        let mut occurrence = site_for_text(reference, records[&2].record.id, &text);
        occurrence.source = super::super::source_for_span(&full, physical);
        occurrence.evidence.source = Some(occurrence.source.clone());
        assert_eq!(
            proof
                .resolve(reference, physical, &occurrence, &Cancellation::new())
                .unwrap(),
            Some(records[&4].record.id)
        );
        assert_eq!(
            proof
                .resolve(
                    reference,
                    reference.span(),
                    &occurrence,
                    &Cancellation::new()
                )
                .unwrap(),
            None
        );
        for mutation in 0..4 {
            let mut changed = occurrence.clone();
            match mutation {
                0 => changed.enclosing = Some(records[&4].record.id),
                1 => changed.provenance = FactId::from_bytes([9; 20]),
                2 => changed.source = source_for_text(physical, "different"),
                3 => changed.generation = GenerationId::from_bytes([9; 20]),
                _ => unreachable!(),
            }
            assert_eq!(
                proof
                    .resolve(reference, physical, &changed, &Cancellation::new())
                    .unwrap(),
                None
            );
        }
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            proof.resolve(reference, physical, &occurrence, &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
        let bom_text = char::from_u32(0xfeff).unwrap().to_string() + ITERABLE_TEXT;
        let (facts, records) = iterable_fixture(&bom_text);
        let reference = &facts[10];
        let occurrence = site_for_text(reference, records[&2].record.id, &bom_text);
        assert_eq!(
            iterable_targets(&bom_text, &facts, &records)
                .resolve(
                    reference,
                    reference.span(),
                    &occurrence,
                    &Cancellation::new()
                )
                .unwrap(),
            Some(records[&4].record.id)
        );
    }
}
