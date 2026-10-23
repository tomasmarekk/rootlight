//! Consume shell wrapper certificates only after a current-source syntax census.
//! The native tree is reconstructed independently of producer labels, then exact
//! declaration, context, definition, ownership and physical-source joins gate Calls.

use std::{collections::HashMap, ops::ControlFlow};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};
use tree_sitter::{Node, ParseOptions};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
    source_for_span, span_contains,
};

pub(super) const CALL: &str = "bash.direct_wrapper_call.call";
pub(super) const CONTEXT: &str = "bash.direct_wrapper_context.signature";

/// Retain the certified invocation and its independent command-context carrier.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    (fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == CALL)
        || (fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str() == CONTEXT)
        || (fact.kind() == SyntaxFactKind::Scope
            && fact.syntax_kind().as_str() == "bash.direct_wrapper_body.scope")
}

fn unique<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|old| *old = None)
        .or_insert(Some(value));
}

fn bytes<'source>(source: &'source [u8], node: Node<'_>) -> Option<&'source [u8]> {
    source.get(node.byte_range())
}

fn identifier(raw: &[u8]) -> Option<&str> {
    let (&first, rest) = raw.split_first()?;
    ((first.is_ascii_alphabetic() || first == b'_')
        && rest
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_'))
    .then(|| std::str::from_utf8(raw).ok())
    .flatten()
}

fn fd(raw: &[u8]) -> bool {
    let Some(position) = raw.windows(2).position(|pair| pair == b">&") else {
        return false;
    };
    raw.get(..position)
        .is_some_and(|left| left.iter().all(u8::is_ascii_digit))
        && raw
            .get(position.saturating_add(2)..)
            .is_some_and(|right| !right.is_empty() && right.iter().all(u8::is_ascii_digit))
}

fn command_name(raw: &[u8]) -> Option<&str> {
    // This scanner does not expand words. Its only executable token is a literal
    // identifier; quoted arguments and descriptor copies remain inert syntax.
    let mut at = 0usize;
    let mut name = None;
    while at < raw.len() {
        while raw.get(at).is_some_and(u8::is_ascii_whitespace) {
            at = at.checked_add(1)?;
        }
        if at == raw.len() {
            break;
        }
        let start = at;
        if matches!(raw.get(at), Some(b'\'' | b'"')) {
            name?;
            let quote = *raw.get(at)?;
            at = at.checked_add(1)?;
            while raw.get(at).is_some_and(|byte| *byte != quote) {
                if raw.get(at).is_some_and(|byte| b"`\\\r\n".contains(byte)) {
                    return None;
                }
                at = at.checked_add(1)?;
            }
            if raw.get(at) != Some(&quote) {
                return None;
            }
            at = at.checked_add(1)?;
            let token = raw.get(start..at)?;
            if quote == b'"' && token != b"\"$@\"" && token.contains(&b'$') {
                return None;
            }
            if raw.get(at).is_some_and(|byte| !byte.is_ascii_whitespace()) {
                return None;
            }
        } else {
            while raw.get(at).is_some_and(|byte| !byte.is_ascii_whitespace()) {
                at = at.checked_add(1)?;
            }
            let token = raw.get(start..at)?;
            if fd(token) {
                continue;
            }
            if name.is_none() {
                let candidate = identifier(token)?;
                if matches!(
                    candidate,
                    "command"
                        | "builtin"
                        | "eval"
                        | "source"
                        | "exec"
                        | "alias"
                        | "unalias"
                        | "unset"
                        | "enable"
                        | "trap"
                ) {
                    return None;
                }
                name = Some(candidate);
            } else if token.is_empty()
                || token
                    .iter()
                    .any(|byte| !byte.is_ascii_alphanumeric() && !b"_./+-:=,@%".contains(byte))
            {
                return None;
            }
        }
    }
    name
}

#[derive(Clone, Copy)]
struct WrittenCall {
    owner: (usize, usize),
    target: (usize, usize),
    owner_header: (usize, usize),
    target_header: (usize, usize),
    target_name: (usize, usize),
    owner_name: (usize, usize),
    body: (usize, usize),
}

struct Work {
    remaining: usize,
}
impl Work {
    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }
}

fn module_owned(
    mut node: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    while let Some(parent) = node.parent() {
        if !work.step(cancel)? {
            return Ok(false);
        }
        match parent.kind() {
            "program" => return Ok(true),
            "compound_statement" if source.get(parent.start_byte()) == Some(&b'{') => {}
            _ => return Ok(false),
        }
        node = parent;
    }
    Ok(false)
}

fn outside_function(
    mut node: Node<'_>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    while let Some(parent) = node.parent() {
        if !work.step(cancel)? {
            return Ok(false);
        }
        match parent.kind() {
            "function_definition" => return Ok(false),
            "program" => return Ok(true),
            _ => {}
        }
        node = parent;
    }
    Ok(false)
}

fn ordinary(
    node: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let (Some(name), Some(body)) = (
        node.child_by_field_name("name"),
        node.child_by_field_name("body"),
    ) else {
        return Ok(false);
    };
    if node.start_byte() != name.start_byte()
        || node.child_by_field_name("redirect").is_some()
        || body.kind() != "compound_statement"
        || source.get(body.start_byte()) != Some(&b'{')
        || source.get(body.end_byte().saturating_sub(1)) != Some(&b'}')
        || bytes(source, name).and_then(identifier).is_none()
        || !module_owned(node, source, work, cancel)?
    {
        return Ok(false);
    }
    let Some(header) = source.get(name.end_byte()..body.start_byte()) else {
        return Ok(false);
    };
    for _ in header.chunks(64) {
        if !work.step(cancel)? {
            return Ok(false);
        }
    }
    let mut punctuation = header
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace());
    Ok(punctuation.next() == Some(b'(')
        && punctuation.next() == Some(b')')
        && punctuation.next().is_none())
}

fn function_header(
    node: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<(usize, usize)>, AdapterError> {
    let Some(body) = node.child_by_field_name("body") else {
        return Ok(None);
    };
    let Some(header) = source.get(node.start_byte()..body.start_byte()) else {
        return Ok(None);
    };
    let mut retained = 0;
    for (index, chunk) in header.chunks(64).enumerate() {
        if !work.step(cancel)? {
            return Ok(None);
        }
        if let Some(last) = chunk.iter().rposition(|byte| !byte.is_ascii_whitespace()) {
            retained = index * 64 + last + 1;
        }
    }
    Ok(Some((node.start_byte(), node.start_byte() + retained)))
}

fn source_calls(
    source: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<HashMap<(usize, usize), WrittenCall>, AdapterError> {
    cancel.check()?;
    let mut result = HashMap::new();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .map_err(|_| provider_failure("bash-wrapper-consumer-grammar"))?;
    let mut parse_work = maximum;
    let mut exhausted = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if cancel.check().is_err() {
            return ControlFlow::Break(());
        }
        match parse_work.checked_sub(1) {
            Some(next) => {
                parse_work = next;
                ControlFlow::Continue(())
            }
            None => {
                exhausted = true;
                ControlFlow::Break(())
            }
        }
    };
    let mut input = |offset: usize, _: tree_sitter::Point| source.get(offset..).unwrap_or_default();
    let tree = parser.parse_with_options(
        &mut input,
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    cancel.check()?;
    if exhausted {
        return Ok(result);
    }
    let Some(tree) = tree else { return Ok(result) };
    let root = tree.root_node();
    if root.kind() != "program"
        || root.has_error()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
    {
        return Ok(result);
    }
    let mut work = Work { remaining: maximum };
    let mut functions = HashMap::new();
    let mut owners = Vec::new();
    let mut earliest_barrier = None;
    let mut cursor = root.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(result);
        }
        let node = cursor.node();
        if node.kind() == "function_definition" {
            let Some(name) = node
                .child_by_field_name("name")
                .and_then(|name| bytes(source, name))
                .and_then(identifier)
            else {
                return Ok(result);
            };
            functions
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            unique(&mut functions, name, node);
            owners
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            owners.push(node);
        }
        if matches!(
            node.kind(),
            "command" | "unset_command" | "declaration_command" | "&"
        ) && outside_function(node, &mut work, cancel)?
        {
            // A module-executed async terminator belongs to the preceding statement,
            // including a whole brace group; source order alone cannot fence it.
            let position = if node.kind() == "&" {
                0
            } else {
                node.start_byte()
            };
            earliest_barrier =
                Some(earliest_barrier.map_or(position, |old: usize| old.min(position)));
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break;
            }
        }
        if cursor.node() == root {
            break;
        }
    }
    for owner in owners {
        if !work.step(cancel)? {
            return Ok(HashMap::new());
        }
        if !ordinary(owner, source, &mut work, cancel)?
            || earliest_barrier.is_some_and(|position| position < owner.start_byte())
        {
            continue;
        }
        let Some(owner_name) = owner.child_by_field_name("name") else {
            continue;
        };
        let Some(owner_text) = bytes(source, owner_name).and_then(identifier) else {
            continue;
        };
        if functions.get(owner_text).copied().flatten() != Some(owner) {
            continue;
        }
        let Some(body) = owner.child_by_field_name("body") else {
            continue;
        };
        let mut statement = None;
        let mut invalid = false;
        let mut body_cursor = body.walk();
        for child in body.children(&mut body_cursor) {
            if !work.step(cancel)? {
                return Ok(HashMap::new());
            }
            if child.kind() == "comment" || matches!(child.kind(), "{" | "}" | ";") {
                continue;
            }
            invalid |= statement.replace(child).is_some();
        }
        let Some(statement) = statement.filter(|_| !invalid) else {
            continue;
        };
        let command = if statement.kind() == "redirected_statement" {
            let mut redirects = statement.walk();
            let Some(command) = statement
                .child_by_field_name("body")
                .filter(|node| node.kind() == "command")
            else {
                continue;
            };
            let mut invalid = false;
            for child in statement.named_children(&mut redirects) {
                if !work.step(cancel)? {
                    return Ok(HashMap::new());
                }
                invalid |= child != command
                    && (child.kind() != "file_redirect" || !bytes(source, child).is_some_and(fd));
            }
            if invalid {
                continue;
            }
            command
        } else if statement.kind() == "command" {
            statement
        } else {
            continue;
        };
        let Some(command_raw) = bytes(source, command) else {
            continue;
        };
        for _ in command_raw.chunks(64) {
            if !work.step(cancel)? {
                return Ok(HashMap::new());
            }
        }
        let Some(name) = command_name(command_raw) else {
            continue;
        };
        let Some(target) = functions.get(name).copied().flatten() else {
            continue;
        };
        if target.end_byte() > owner.start_byte() || !ordinary(target, source, &mut work, cancel)? {
            continue;
        }
        let Some(target_name) = target.child_by_field_name("name") else {
            continue;
        };
        let (Some(owner_header), Some(target_header)) = (
            function_header(owner, source, &mut work, cancel)?,
            function_header(target, source, &mut work, cancel)?,
        ) else {
            continue;
        };
        result
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        result.insert(
            (command.start_byte(), command.end_byte()),
            WrittenCall {
                owner: (owner.start_byte(), owner.end_byte()),
                target: (target.start_byte(), target.end_byte()),
                owner_header,
                target_header,
                target_name: (target_name.start_byte(), target_name.end_byte()),
                owner_name: (owner_name.start_byte(), owner_name.end_byte()),
                body: (body.start_byte(), body.end_byte()),
            },
        );
    }
    if work.remaining == 0 {
        result.clear();
    }
    Ok(result)
}

fn range(span: SourceSpan) -> Option<(usize, usize)> {
    Some((
        usize::try_from(span.start_byte()).ok()?,
        usize::try_from(span.end_byte()).ok()?,
    ))
}

fn chain(
    fact: &SyntaxFact,
    ancestor: u64,
    all: &HashMap<u64, Option<&SyntaxFact>>,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut child = fact;
    for _ in 0..all.len() {
        cancel.check()?;
        let Some(parent) = child
            .parent()
            .and_then(|id| all.get(&id))
            .copied()
            .flatten()
        else {
            return Ok(false);
        };
        if !span_contains(parent.span(), child.span())
            || parent.depth().checked_add(1) != Some(child.depth())
        {
            return Ok(false);
        }
        if parent.local_id() == ancestor {
            return Ok(true);
        }
        if !matches!(
            parent.kind(),
            SyntaxFactKind::Scope | SyntaxFactKind::Module
        ) {
            return Ok(false);
        }
        if !matches!(
            parent.syntax_kind().as_str(),
            "bash.block.scope"
                | "bash.direct_wrapper_body.scope"
                | "bash.function.scope"
                | "bash.file.scope"
                | "bash.file.module"
        ) {
            return Ok(false);
        }
        child = parent;
    }
    Ok(false)
}

struct Binding {
    owner_local: u64,
    owner: SymbolId,
    target: SymbolId,
    span: SourceSpan,
    hash: ContentHash,
    provenance: FactId,
}

pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl Targets {
    #[expect(
        clippy::too_many_arguments,
        reason = "logical source and exact physical projection are independently checked"
    )]
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        current: &[u8],
        physical_bytes: &[u8],
        maximum: usize,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        if source.span().start_byte() != 0
            || u64::try_from(physical_bytes.len()).ok() != Some(source.span().end_byte())
        {
            return Ok(result);
        }
        let mut source_hash = blake3::Hasher::new();
        for chunk in physical_bytes.chunks(64) {
            cancel.check()?;
            source_hash.update(chunk);
        }
        if ContentHash::from_bytes(*source_hash.finalize().as_bytes()) != source.content_hash() {
            return Ok(result);
        }
        let plan = source_calls(current, maximum, cancel)?;
        let mut headers = HashMap::new();
        headers
            .try_reserve(plan.len().saturating_mul(2))
            .map_err(|_| SinkError::AllocationFailed)?;
        for written in plan.values() {
            cancel.check()?;
            headers.insert(written.owner, written.owner_header);
            headers.insert(written.target, written.target_header);
        }
        let mut all = HashMap::new();
        let mut declarations = HashMap::new();
        let mut definitions = HashMap::new();
        let mut function_scopes = HashMap::new();
        let mut signatures = HashMap::new();
        let mut calls = HashMap::new();
        let mut markers = HashMap::new();
        let mut entity_ids = HashMap::new();
        let mut structural = HashMap::new();
        all.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        structural
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for map in [
            &mut declarations,
            &mut definitions,
            &mut function_scopes,
            &mut signatures,
            &mut calls,
            &mut markers,
        ] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        entity_ids
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut root = None;
        let mut module = None;
        let mut invalid = false;
        for fact in facts {
            cancel.check()?;
            unique(&mut all, fact.local_id(), fact);
            if matches!(
                fact.kind(),
                SyntaxFactKind::Root
                    | SyntaxFactKind::Module
                    | SyntaxFactKind::Scope
                    | SyntaxFactKind::Declaration
                    | SyntaxFactKind::Signature
            ) || (fact.kind() == SyntaxFactKind::Occurrence
                && (fact.syntax_kind().as_str().ends_with(".definition")
                    || fact.syntax_kind().as_str() == CALL))
            {
                unique(
                    &mut structural,
                    (fact.kind(), fact.syntax_kind().as_str(), fact.span()),
                    fact.local_id(),
                );
            }
            invalid |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            let span = fact.span();
            if fact.kind() == SyntaxFactKind::Root {
                invalid |= root.replace(fact).is_some()
                    || fact.syntax_kind().as_str() != "bash.file.root"
                    || fact.parent().is_some()
                    || fact.depth() != 0
                    || physical(span)?.end_byte() != source.span().end_byte()
                    || !matches!(
                        current.get(..usize::try_from(span.start_byte()).unwrap_or(usize::MAX)),
                        Some(b"" | b"\xef\xbb\xbf")
                    );
            }
            if fact.kind() == SyntaxFactKind::Module
                && fact.syntax_kind().as_str() == "bash.file.module"
            {
                invalid |= module.replace(fact).is_some();
            }
            if fact.kind() == SyntaxFactKind::Declaration {
                unique(&mut declarations, span, fact);
            }
            if fact.kind() == SyntaxFactKind::Scope
                && fact.syntax_kind().as_str() == "bash.function.scope"
            {
                unique(&mut function_scopes, span, fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str().ends_with(".definition")
            {
                unique(&mut definitions, span, fact);
            }
            if fact.kind() == SyntaxFactKind::Signature {
                if fact.syntax_kind().as_str() == CONTEXT {
                    unique(&mut markers, span, fact);
                } else {
                    unique(&mut signatures, span, fact);
                }
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && (fact.syntax_kind().as_str() == CALL
                    || fact.syntax_kind().as_str() == "bash.call.call")
            {
                unique(&mut calls, span, fact);
            }
        }
        for (local, entity) in entities {
            cancel.check()?;
            unique(&mut entity_ids, entity.record.id, *local);
        }
        let Some(root) = root.filter(|_| {
            !invalid
                && all.values().all(Option::is_some)
                && structural.values().all(Option::is_some)
        }) else {
            return Ok(result);
        };
        let Some(module) = module.filter(|module| {
            module.span() == root.span()
                && module.parent() == Some(root.local_id())
                && module.depth() == 1
        }) else {
            return Ok(result);
        };
        let module_source = source_for_span(source, physical(module.span())?);
        let Some(module_entity) = entities.get(&module.local_id()).filter(|entity| {
            entity.record.language == "bash"
                && entity.record.kind == EntityKind::Module
                && entity.record.repository == source.repository()
                && entity.record.generation == source.generation()
                && entity.record.evidence.source.as_ref() == Some(&module_source)
                && entity.direct_parent == ContainerRef::File(source.span().file())
                && entity.record.container == Some(entity.direct_parent)
        }) else {
            return Ok(result);
        };
        let mut validated = HashMap::new();
        validated
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            cancel.check()?;
            if entity.record.language != "bash"
                || entity.record.kind != EntityKind::Function
                || entity_ids.get(&entity.record.id) != Some(&Some(*local))
            {
                continue;
            }
            let Some(declaration) = all.get(local).copied().flatten() else {
                continue;
            };
            let Some(span) = range(declaration.span()) else {
                continue;
            };
            let Some(function_scope) = function_scopes.get(&declaration.span()).copied().flatten()
            else {
                continue;
            };
            let Some(&(header_start, header_end)) = headers.get(&span) else {
                continue;
            };
            let header_span = SourceSpan::new(
                declaration.span().file(),
                u64::try_from(header_start)
                    .map_err(|_| provider_failure("bash-wrapper-header-span"))?,
                u64::try_from(header_end)
                    .map_err(|_| provider_failure("bash-wrapper-header-span"))?,
            )
            .map_err(|_| provider_failure("bash-wrapper-header-span"))?;
            let Some(signature) = signatures.get(&header_span).copied().flatten() else {
                continue;
            };
            let Some(definition) = entity
                .definition_local_id
                .and_then(|id| all.get(&id))
                .copied()
                .flatten()
            else {
                continue;
            };
            if declaration.kind() != SyntaxFactKind::Declaration
                || declaration.syntax_kind().as_str() != "bash.function.declaration"
                || declaration.parent() != Some(function_scope.local_id())
                || function_scope.parent() == Some(*local)
                || function_scope.depth().checked_add(1) != Some(declaration.depth())
                || !chain(function_scope, root.local_id(), &all, cancel)?
                || declarations
                    .get(&declaration.span())
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(*local)
                || signature.syntax_kind().as_str() != "bash.function.signature"
                || signature.parent() != Some(*local)
                || signature.depth() != declaration.depth().saturating_add(1)
                || signature.span() != header_span
                || definition.kind() != SyntaxFactKind::Occurrence
                || definition.syntax_kind().as_str() != "bash.identifier.definition"
                || definition.parent() != Some(*local)
                || definition.depth() != signature.depth()
                || definitions
                    .get(&definition.span())
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(definition.local_id())
                || entity.record.repository != source.repository()
                || entity.record.generation != source.generation()
                || entity.record.evidence.source.as_ref()
                    != Some(&source_for_span(source, physical(declaration.span())?))
                || entity.record.canonical_name != text(definition.span())?
                || entity.record.container != Some(entity.direct_parent)
                || entity.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || entity.record.provenance != module_entity.record.provenance
                || !chain(declaration, root.local_id(), &all, cancel)?
                || !span_contains(declaration.span(), definition.span())
            {
                continue;
            }
            validated.insert(span, (*local, entity, definition));
        }
        for (span, marker) in markers
            .iter()
            .filter_map(|(span, marker)| marker.map(|marker| (*span, marker)))
        {
            cancel.check()?;
            let Some(written) = range(span).and_then(|range| plan.get(&range)) else {
                continue;
            };
            let Some(call) = calls
                .get(&span)
                .copied()
                .flatten()
                .filter(|call| call.syntax_kind().as_str() == CALL)
            else {
                continue;
            };
            let (
                Some(&(owner_local, owner, owner_definition)),
                Some(&(_, target, target_definition)),
            ) = (
                validated.get(&written.owner),
                validated.get(&written.target),
            )
            else {
                continue;
            };
            if range(owner_definition.span()) != Some(written.owner_name)
                || range(target_definition.span()) != Some(written.target_name)
                || call.parent() != marker.parent()
                || call.depth() != marker.depth()
                || call.local_id() == marker.local_id()
                || !chain(call, owner_local, &all, cancel)?
                || !chain(marker, owner_local, &all, cancel)?
                || owner.direct_parent != target.direct_parent
                || owner.record.provenance != target.record.provenance
                || text(span)?.as_bytes()
                    != current
                        .get(
                            usize::try_from(span.start_byte()).unwrap_or(usize::MAX)
                                ..usize::try_from(span.end_byte()).unwrap_or(usize::MAX),
                        )
                        .unwrap_or_default()
                || command_name(text(span)?.as_bytes())
                    != Some(target.record.canonical_name.as_str())
            {
                continue;
            }
            let Some(parent) = call.parent().and_then(|id| all.get(&id)).copied().flatten() else {
                continue;
            };
            if parent.kind() != SyntaxFactKind::Scope
                || parent.syntax_kind().as_str() != "bash.direct_wrapper_body.scope"
                || range(parent.span()) != Some(written.body)
            {
                continue;
            }
            let physical_span = physical(span)?;
            result
                .bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.bindings.insert(
                call.local_id(),
                Binding {
                    owner_local,
                    owner: owner.record.id,
                    target: target.record.id,
                    span: physical_span,
                    hash: content_hash(text(span)?.as_bytes()),
                    provenance: owner.record.provenance,
                },
            );
        }
        Ok(result)
    }

    pub(super) fn owner<'a>(
        &self,
        fact: &SyntaxFact,
        entities: &'a HashMap<u64, MaterializedEntity>,
        cancel: &Cancellation,
    ) -> Result<Option<&'a MaterializedEntity>, AdapterError> {
        cancel.check()?;
        Ok(self.bindings.get(&fact.local_id()).and_then(|binding| {
            entities.get(&binding.owner_local).filter(|owner| {
                owner.record.id == binding.owner && owner.record.provenance == binding.provenance
            })
        }))
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical_span: SourceSpan,
        site: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != CALL
            || site.syntax_kind != CALL
            || site.role != OccurrenceRole::CallSite
            || site.enclosing != Some(binding.owner)
            || owner.is_none_or(|owner| owner.record.id != binding.owner)
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
            || site.source != source_for_span(&self.source, binding.span)
            || physical_span != binding.span
            || site.provenance != binding.provenance
            || content_hash(text.as_bytes()) != binding.hash
            || site.syntactic_text_hash != binding.hash
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
        {
            return Ok(None);
        }
        Ok(Some(binding.target))
    }
}

/// Produce the source-owned Calls edge after exact occurrence resolution.
pub(super) fn relation(
    site: &OccurrenceRecord,
    owner: &MaterializedEntity,
    target: SymbolId,
    cancel: &Cancellation,
) -> Result<RelationRecord, AdapterError> {
    cancel.check()?;
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
        .map_err(|_| provider_failure("bash-wrapper-call-identity"))?;
    Ok(relation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_source_census_is_independent_of_native_certificate_labels() {
        let spaced = b"target()  { :; }\nwrapper() \t { target; }\n";
        let spaced_plan = source_calls(spaced, 16384, &Cancellation::new()).unwrap();
        assert_eq!(spaced_plan.len(), 1);
        let written = spaced_plan.values().next().unwrap();
        assert_eq!(written.target_header, (0, 8));
        assert_eq!(written.owner_header, (17, 26));
        for source in [
            b"target(){ :; }\nwrapper(){ >&2 target \"$@\"; }\n".as_slice(),
            b"{ target(){ :; }\nwrapper(){ target \"$@\" >&2; }\n}\n".as_slice(),
        ] {
            assert_eq!(
                source_calls(source, 16384, &Cancellation::new())
                    .unwrap()
                    .len(),
                1
            );
        }
        for source in [
            "target(){ :; }\nwrapper(){ target \"$(other)\"; }\n",
            "target(){ :; }\nwrapper(){ target; }\ntarget(){ :; }\n",
            "target(){ :; }\nwrapper(){ target & }\n",
            "target(){ :; }\nwrapper(){ X=x target; }\n",
        ] {
            assert!(
                source_calls(source.as_bytes(), 16384, &Cancellation::new())
                    .unwrap()
                    .is_empty(),
                "{source}"
            );
        }
        let source = include_bytes!("../../tests/fixtures/bash_written_calls/nvm.sh");
        let plan = source_calls(source, 1_048_576, &Cancellation::new()).unwrap();
        for span in [(732, 749), (779, 808)] {
            assert!(plan.contains_key(&span));
        }
        assert!(
            source_calls(source, 1, &Cancellation::new())
                .unwrap()
                .is_empty()
        );
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            source_calls(source, 16384, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn independent_current_source_census_refuses_valid_module_async_terminators() {
        for source in [
            "target(){ :; }\nwrapper(){ target; } &\n",
            "{ target(){ :; }; wrapper(){ target; }; } &\n",
            "target(){ :; } &\nwrapper(){ target; }\n",
            "target(){ :; }\nwrapper(){ target; }\nexternal &\n",
        ] {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_bash::LANGUAGE.into())
                .unwrap();
            assert!(
                !parser.parse(source, None).unwrap().root_node().has_error(),
                "valid async syntax premise: {source}"
            );
            assert!(
                source_calls(source.as_bytes(), 16384, &Cancellation::new())
                    .unwrap()
                    .is_empty(),
                "{source}"
            );
        }
        let uncalled = "target(){ :; }\nwrapper(){ target; }\nother(){ external & }\n";
        assert_eq!(
            source_calls(uncalled.as_bytes(), 16384, &Cancellation::new())
                .unwrap()
                .len(),
            1
        );
    }

    const TEXT: &str = "target(){ :; }\nwrapper(){ >&2 target \"$@\"; }\n";

    fn span(start: usize, end: usize) -> SourceSpan {
        SourceSpan::new(
            rootlight_ids::FileId::from_bytes([2; 20]),
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        )
        .unwrap()
    }

    fn frame() -> SourceRef {
        SourceRef::new(
            rootlight_ids::RepositoryId::from_bytes([1; 16]),
            rootlight_ids::GenerationId::from_bytes([3; 20]),
            span(0, TEXT.len()),
            content_hash(TEXT.as_bytes()),
            None,
        )
    }

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        start: usize,
        end: usize,
        depth: usize,
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            span(start, end),
            depth,
            rootlight_adapter_sdk::SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn witnesses() -> Vec<SyntaxFact> {
        use SyntaxFactKind::{Declaration, Module, Occurrence, Root, Scope, Signature};
        let target_end = TEXT.find('\n').unwrap();
        let owner = target_end + 1;
        let end = TEXT.len() - 1;
        let body = TEXT[owner..].find('{').unwrap() + owner;
        let command = TEXT.find(">&2").unwrap();
        let command_end = TEXT[command..].find(';').unwrap() + command;
        vec![
            fact(1, None, Root, 0, TEXT.len(), 0, "bash.file.root"),
            fact(2, Some(1), Module, 0, TEXT.len(), 1, "bash.file.module"),
            fact(3, Some(2), Scope, 0, TEXT.len(), 2, "bash.file.scope"),
            fact(4, Some(3), Scope, 0, target_end, 3, "bash.function.scope"),
            fact(
                5,
                Some(4),
                Declaration,
                0,
                target_end,
                4,
                "bash.function.declaration",
            ),
            fact(
                6,
                Some(5),
                Occurrence,
                0,
                6,
                5,
                "bash.identifier.definition",
            ),
            fact(
                7,
                Some(5),
                Signature,
                0,
                TEXT.find('{').unwrap(),
                5,
                "bash.function.signature",
            ),
            fact(9, Some(3), Scope, owner, end, 3, "bash.function.scope"),
            fact(
                10,
                Some(9),
                Declaration,
                owner,
                end,
                4,
                "bash.function.declaration",
            ),
            fact(
                11,
                Some(10),
                Occurrence,
                owner,
                owner + 7,
                5,
                "bash.identifier.definition",
            ),
            fact(
                12,
                Some(10),
                Signature,
                owner,
                body,
                5,
                "bash.function.signature",
            ),
            fact(
                13,
                Some(10),
                Scope,
                body,
                end,
                5,
                "bash.direct_wrapper_body.scope",
            ),
            fact(14, Some(13), Signature, command, command_end, 6, CONTEXT),
            fact(15, Some(13), Occurrence, command, command_end, 6, CALL),
        ]
    }

    fn materialized() -> HashMap<u64, MaterializedEntity> {
        use rootlight_ir::{
            AnalysisTier, EntityRecord, EntityVisibility, FactEvidence, SymbolIdentityClaim,
        };
        let source = frame();
        let raw = witnesses();
        let provenance = FactId::from_bytes([4; 20]);
        let mut entities = HashMap::new();
        for (local, definition, kind, name) in [
            (2, None, EntityKind::Module, "commands"),
            (5, Some(6), EntityKind::Function, "target"),
            (10, Some(11), EntityKind::Function, "wrapper"),
        ] {
            let parent = if kind == EntityKind::Module {
                ContainerRef::File(source.span().file())
            } else {
                ContainerRef::Entity(
                    entities
                        .get(&2)
                        .map(|entity: &MaterializedEntity| entity.record.id)
                        .unwrap(),
                )
            };
            let mut claim = SymbolIdentityClaim {
                symbol: SymbolId::from_bytes([0; 20]),
                repository: source.repository(),
                language: "bash".to_owned(),
                kind,
                container: Some(parent),
                container_identity: b"written-shell-container".to_vec(),
                declared_identity: name.to_owned(),
                signature_discriminator: Vec::new(),
                build_context_discriminator: Vec::new(),
            };
            claim.symbol = claim.derived_symbol();
            let record = EntityRecord {
                id: claim.symbol,
                repository: source.repository(),
                generation: source.generation(),
                kind,
                language: "bash".to_owned(),
                tier: AnalysisTier::TierD,
                canonical_name: name.to_owned(),
                display_name: name.to_owned(),
                qualified_name: name.to_owned(),
                container: Some(parent),
                visibility: EntityVisibility::Unknown,
                flags: Vec::new(),
                provenance,
                evidence: FactEvidence {
                    source: Some(source_for_span(
                        &source,
                        raw.iter()
                            .find(|fact| fact.local_id() == local)
                            .unwrap()
                            .span(),
                    )),
                    derivation: Vec::new(),
                },
            };
            entities.insert(
                local,
                MaterializedEntity {
                    record,
                    identity_claim: claim,
                    direct_parent: parent,
                    identity_guard: [0; 32],
                    definition_local_id: definition,
                    signature_evidence: None,
                    signature_span: None,
                },
            );
        }
        entities
    }

    fn targets(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        text: &str,
    ) -> Targets {
        Targets::new(
            facts,
            entities,
            source,
            text.as_bytes(),
            text.as_bytes(),
            16384,
            |span| {
                Ok(&text[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()])
            },
            Ok,
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn raw_cardinality_exact_depth_scope_module_and_current_bytes_are_jointly_required() {
        let original = witnesses();
        let entities = materialized();
        assert_eq!(
            targets(&original, &entities, &frame(), TEXT).bindings.len(),
            1
        );
        for missing in 0..original.len() {
            let mut altered = original.clone();
            altered.remove(missing);
            assert!(
                targets(&altered, &entities, &frame(), TEXT)
                    .bindings
                    .is_empty(),
                "missing={missing}"
            );
        }
        for duplicate in 0..original.len() {
            let mut altered = original.clone();
            let old = &original[duplicate];
            altered.push(fact(
                99,
                old.parent(),
                old.kind(),
                usize::try_from(old.span().start_byte()).unwrap(),
                usize::try_from(old.span().end_byte()).unwrap(),
                old.depth(),
                old.syntax_kind().as_str(),
            ));
            assert!(
                targets(&altered, &entities, &frame(), TEXT)
                    .bindings
                    .is_empty(),
                "duplicate={duplicate}"
            );
        }
        for changed in 0..original.len() {
            for shape in 0..4 {
                if shape == 0 && matches!(original[changed].local_id(), 7 | 12 | 14 | 15) {
                    continue;
                }
                let old = &original[changed];
                let mut altered = original.clone();
                altered[changed] = fact(
                    if shape == 0 { 99 } else { old.local_id() },
                    if shape == 1 {
                        old.parent().is_none().then_some(old.local_id())
                    } else {
                        old.parent()
                    },
                    old.kind(),
                    usize::try_from(old.span().start_byte()).unwrap(),
                    usize::try_from(old.span().end_byte()).unwrap(),
                    if shape == 2 {
                        old.depth() + 1
                    } else {
                        old.depth()
                    },
                    if shape == 3 {
                        "other.foreign.reference"
                    } else {
                        old.syntax_kind().as_str()
                    },
                );
                assert!(
                    targets(&altered, &entities, &frame(), TEXT)
                        .bindings
                        .is_empty(),
                    "changed={changed},shape={shape}"
                );
            }
        }
        for key in [2, 5, 10] {
            let mut altered = entities.clone();
            altered.remove(&key);
            assert!(
                targets(&original, &altered, &frame(), TEXT)
                    .bindings
                    .is_empty(),
                "missing entity {key}"
            );
            for shape in 0..6 {
                let mut altered = entities.clone();
                let entity = altered.get_mut(&key).unwrap();
                match shape {
                    0 => {
                        entity.record.generation = rootlight_ids::GenerationId::from_bytes([9; 20])
                    }
                    1 => entity.record.provenance = FactId::from_bytes([9; 20]),
                    2 => entity.record.language = "foreign".to_owned(),
                    3 => entity.record.evidence.source = None,
                    4 => entity.direct_parent = ContainerRef::File(frame().span().file()),
                    _ => entity.record.canonical_name = "forged".to_owned(),
                }
                // Module names and its unchanged file container are irrelevant to the function dependency.
                if key == 2 && matches!(shape, 4 | 5) {
                    continue;
                }
                assert!(
                    targets(&original, &altered, &frame(), TEXT)
                        .bindings
                        .is_empty(),
                    "entity={key},shape={shape}"
                );
            }
        }
        let changed = TEXT.replacen("target", "absent", 1);
        let changed_frame = SourceRef::new(
            frame().repository(),
            frame().generation(),
            span(0, changed.len()),
            content_hash(changed.as_bytes()),
            None,
        );
        assert!(
            targets(&original, &entities, &changed_frame, &changed)
                .bindings
                .is_empty()
        );
    }

    #[test]
    fn both_function_signatures_require_exact_current_source_headers() {
        let original = witnesses();
        let entities = materialized();
        assert_eq!(
            targets(&original, &entities, &frame(), TEXT).bindings.len(),
            1
        );
        for signature_id in [7, 12] {
            let signature = original
                .iter()
                .find(|fact| fact.local_id() == signature_id)
                .unwrap();
            let declaration = original
                .iter()
                .find(|fact| Some(fact.local_id()) == signature.parent())
                .unwrap();
            for (start, end) in [
                (
                    declaration.span().start_byte(),
                    declaration.span().end_byte(),
                ),
                (
                    signature.span().start_byte() + 1,
                    signature.span().end_byte(),
                ),
                (
                    signature.span().start_byte(),
                    signature.span().end_byte() - 1,
                ),
            ] {
                let mut altered = original.clone();
                let index = altered
                    .iter()
                    .position(|fact| fact.local_id() == signature_id)
                    .unwrap();
                altered[index] = fact(
                    signature.local_id(),
                    signature.parent(),
                    signature.kind(),
                    usize::try_from(start).unwrap(),
                    usize::try_from(end).unwrap(),
                    signature.depth(),
                    signature.syntax_kind().as_str(),
                );
                assert!(
                    targets(&altered, &entities, &frame(), TEXT)
                        .bindings
                        .is_empty(),
                    "header={signature_id},range={start}..{end}"
                );
            }
        }
    }

    #[test]
    fn both_function_scopes_are_exact_singular_declaration_parents() {
        let original = witnesses();
        let entities = materialized();
        assert_eq!(
            targets(&original, &entities, &frame(), TEXT).bindings.len(),
            1
        );
        for (scope_id, declaration_id) in [(4, 5), (9, 10)] {
            let scope = original
                .iter()
                .find(|fact| fact.local_id() == scope_id)
                .unwrap();
            for mutation in 0..6 {
                let mut changed = original.clone();
                match mutation {
                    0 => {
                        let index = changed
                            .iter()
                            .position(|fact| fact.local_id() == scope_id)
                            .unwrap();
                        changed[index] = fact(
                            scope_id,
                            scope.parent(),
                            scope.kind(),
                            0,
                            TEXT.len(),
                            scope.depth(),
                            scope.syntax_kind().as_str(),
                        );
                    }
                    1 => changed.retain(|fact| fact.local_id() != scope_id),
                    2 | 3 => {
                        if mutation == 2 {
                            changed.retain(|fact| fact.local_id() != scope_id);
                        }
                        for old in &original {
                            let mut ancestor = old.parent();
                            let mut inside = false;
                            for _ in 0..original.len() {
                                if ancestor == Some(scope_id) {
                                    inside = true;
                                    break;
                                }
                                ancestor = ancestor
                                    .and_then(|parent| {
                                        original.iter().find(|fact| fact.local_id() == parent)
                                    })
                                    .and_then(SyntaxFact::parent);
                            }
                            if !inside {
                                continue;
                            }
                            let index = changed
                                .iter()
                                .position(|fact| fact.local_id() == old.local_id())
                                .unwrap();
                            changed[index] = fact(
                                old.local_id(),
                                if old.local_id() == declaration_id {
                                    scope.parent()
                                } else {
                                    old.parent()
                                },
                                old.kind(),
                                usize::try_from(old.span().start_byte()).unwrap(),
                                usize::try_from(old.span().end_byte()).unwrap(),
                                old.depth() - 1,
                                old.syntax_kind().as_str(),
                            );
                        }
                    }
                    4 => changed.push(fact(
                        99,
                        scope.parent(),
                        scope.kind(),
                        usize::try_from(scope.span().start_byte()).unwrap(),
                        usize::try_from(scope.span().end_byte()).unwrap(),
                        scope.depth(),
                        scope.syntax_kind().as_str(),
                    )),
                    _ => {
                        let index = changed
                            .iter()
                            .position(|fact| fact.local_id() == scope_id)
                            .unwrap();
                        changed[index] = fact(
                            scope_id,
                            scope.parent(),
                            scope.kind(),
                            usize::try_from(scope.span().start_byte()).unwrap(),
                            usize::try_from(scope.span().end_byte()).unwrap(),
                            scope.depth(),
                            "bash.block.scope",
                        );
                    }
                }
                assert!(
                    targets(&changed, &entities, &frame(), TEXT)
                        .bindings
                        .is_empty(),
                    "scope={scope_id},mutation={mutation}"
                );
            }
        }
    }

    #[test]
    fn final_occurrence_frame_hash_role_and_unresolved_target_are_checked_before_resolution() {
        let raw = witnesses();
        let entities = materialized();
        let source = frame();
        let plan = targets(&raw, &entities, &source, TEXT);
        let call = raw.iter().find(|fact| fact.local_id() == 15).unwrap();
        let owner = plan
            .owner(call, &entities, &Cancellation::new())
            .unwrap()
            .unwrap();
        let text = &TEXT[usize::try_from(call.span().start_byte()).unwrap()
            ..usize::try_from(call.span().end_byte()).unwrap()];
        let site = super::super::unresolved_occurrence(
            call,
            OccurrenceRole::CallSite,
            Some(owner.record.id),
            owner.record.provenance,
            rootlight_ir::Confidence::new(900).unwrap(),
            source_for_span(&source, call.span()),
            text,
        )
        .unwrap();
        let resolve = |site: &OccurrenceRecord| {
            plan.resolve(
                call,
                call.span(),
                site,
                Some(owner),
                text,
                &Cancellation::new(),
            )
            .unwrap()
        };
        let target = resolve(&site).unwrap();
        for shape in 0..7 {
            let mut changed = site.clone();
            match shape {
                0 => changed.role = OccurrenceRole::Reference,
                1 => changed.enclosing = None,
                2 => changed.provenance = FactId::from_bytes([9; 20]),
                3 => changed.syntactic_text_hash = content_hash(b"forged"),
                4 => changed.target = OccurrenceTarget::Resolved { symbol: target },
                5 => {
                    changed.source = SourceRef::new(
                        source.repository(),
                        source.generation(),
                        call.span(),
                        content_hash(b"forged"),
                        None,
                    )
                }
                _ => changed.syntax_kind = "bash.call.call".to_owned(),
            }
            assert!(resolve(&changed).is_none(), "shape={shape}");
        }
        let relation = relation(&site, owner, target, &Cancellation::new()).unwrap();
        assert_eq!(relation.evidence.derivation.len(), 3);
        assert_eq!(relation.id, derive_relation_record_id(&relation).unwrap());
    }
}

#[cfg(test)]
mod admission_tests {
    use std::{fs, path::Path, sync::Arc};

    use super::{CALL, CONTEXT};
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
    fn native_module_call_preflight_reserves_relation_and_three_proof_parents() {
        let source = "target(){ :; }\nwrapper(){ >&2 target \"$@\"; }\n";
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
                .filter(|fact| fact.syntax_kind().as_str() == "bash.direct_wrapper_call.call")
                .count(),
            1
        );
        let entity_count = parsed
            .facts()
            .iter()
            .filter(|fact| structural_entity_kind(fact).is_some())
            .count();
        // Preflight reserves a possible Definition for each entity before
        // capture association, beside every explicit occurrence candidate.
        let occurrence_count = entity_count
            + parsed
                .facts()
                .iter()
                .filter(|fact| occurrence_role(fact).is_some())
                .count();
        let relation_count = entity_count + 1;
        let mut exact_ir = IrLimits::default();
        exact_ir.max_relations = relation_count;
        exact_ir.max_occurrences = occurrence_count;
        let exact_limits = limits(exact_ir);
        let exact_request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
            LanguageId::new("bash").unwrap(),
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
        let exact = execute_analysis(
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
            exact
                .document()
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Calls)
                .count(),
            1
        );
        for missing_context in [true, false] {
            let mut facts = parsed.facts().to_vec();
            if missing_context {
                facts.retain(|fact| fact.syntax_kind().as_str() != CONTEXT);
            } else {
                let index = facts
                    .iter()
                    .position(|fact| fact.syntax_kind().as_str() == "bash.function.signature")
                    .unwrap();
                let signature = facts[index].clone();
                let declaration = facts
                    .iter()
                    .find(|fact| Some(fact.local_id()) == signature.parent())
                    .unwrap();
                facts[index] = SyntaxFact::new(
                    signature.local_id(),
                    signature.parent(),
                    signature.kind(),
                    declaration.span(),
                    signature.depth(),
                    signature.syntax_kind().clone(),
                );
            }
            let replay = rootlight_adapter_sdk::testkit::MockParseProvider::new(
                analyzer.parser.capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            )
            .with_syntax_nodes(parsed.report().resources().syntax_nodes());
            let replay_analyzer = TreeSitterAnalyzer::new(
                Arc::new(replay),
                producer.clone(),
                LanguageId::new("bash").unwrap(),
                "native-module-capacity",
                content_hash(b"capacity-binary"),
            )
            .unwrap();
            let refused = execute_analysis(
                &replay_analyzer,
                &request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancellation,
            )
            .unwrap();
            assert_eq!(
                refused
                    .document()
                    .entities
                    .iter()
                    .filter(|entity| entity.kind == EntityKind::Function)
                    .count(),
                2
            );
            let sites: Vec<_> = refused
                .document()
                .occurrences
                .iter()
                .filter(|site| site.syntax_kind == CALL)
                .collect();
            assert_eq!(sites.len(), 1);
            assert!(matches!(
                sites[0].target,
                OccurrenceTarget::Unresolved { .. }
            ));
            assert!(sites[0].enclosing.is_some());
            assert_eq!(sites[0].source.content_hash(), snapshot.content_hash());
            assert!(
                !refused
                    .document()
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Calls)
            );
        }
        for variant in 0..3 {
            let mut ir = IrLimits::default();
            if variant == 0 {
                ir.max_nested_items_per_record = 2;
            } else if variant == 1 {
                ir.max_relations = entity_count;
            } else {
                ir.max_occurrences = occurrence_count - 1;
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
            if variant == 0 {
                assert!(matches!(
                    rejected,
                    Err(AdapterError::Sink(SinkError::StreamLimit {
                        resource: ResourceKind::NestedItems,
                        observed: 3,
                        limit: 2
                    }))
                ));
            } else if variant == 1 {
                assert!(
                    matches!(rejected, Err(AdapterError::Sink(SinkError::StreamLimit {resource: ResourceKind::Records, observed, limit})) if observed == relation_count && limit == relation_count - 1)
                );
            } else {
                assert!(
                    matches!(rejected, Err(AdapterError::Sink(SinkError::StreamLimit {resource: ResourceKind::Records, observed, limit})) if observed == occurrence_count && limit == occurrence_count - 1)
                );
            }
        }
    }
}
