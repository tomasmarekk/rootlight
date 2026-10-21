//! Join closed-standalone SML formal certificates to exact current native owners.
//! Pattern status is supplied by the producer's declared initial-basis profile;
//! missing, competing or foreign raw witnesses never acquire a lexical target.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::MaterializedEntity;

/// Intersect native sites with current written value-token positions.
/// The first unmodeled binder/type context vetoes later sites; comments and
/// strings never donate identifiers. Only requested fact spans allocate entries.
fn source_positions(
    body: &str,
    base: u64,
    file: rootlight_ids::FileId,
    mut requested: impl FnMut(SourceSpan) -> Result<Option<SourceSpan>, AdapterError>,
    cancel: &Cancellation,
) -> Result<std::collections::HashSet<SourceSpan>, AdapterError> {
    let bytes = body.as_bytes();
    let mut positions = std::collections::HashSet::new();
    let mut at = 0_usize;
    let mut last_position = None;
    while at < bytes.len() {
        cancel.check()?;
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if bytes[at..].starts_with(b"(*") {
            at += 2;
            let mut depth = 1_usize;
            while depth != 0 {
                cancel.check()?;
                if bytes.get(at..).is_some_and(|tail| tail.starts_with(b"(*")) {
                    let Some(next) = depth.checked_add(1).filter(|depth| *depth <= 128) else {
                        return Ok(positions);
                    };
                    depth = next;
                    at += 2;
                } else if bytes.get(at..).is_some_and(|tail| tail.starts_with(b"*)")) {
                    depth -= 1;
                    at += 2;
                } else if at < bytes.len() {
                    at += 1;
                } else {
                    return Ok(positions);
                }
            }
            continue;
        }
        if bytes[at] == b'"' {
            at += 1;
            while at < bytes.len() && bytes[at] != b'"' {
                cancel.check()?;
                if bytes[at] == b'\\' {
                    at += 1;
                    if at == bytes.len() {
                        return Ok(positions);
                    }
                }
                at += 1;
            }
            if at == bytes.len() {
                return Ok(positions);
            }
            at += 1;
            continue;
        }
        if bytes[at] == b'.' {
            if let Some(span) = last_position {
                positions.remove(&span);
            }
            break;
        }
        if matches!(bytes[at], b':' | b'#' | b'{' | b'[' | b'\'') || !bytes[at].is_ascii() {
            break;
        }
        if bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' {
            last_position = None;
            let start = at;
            while at < bytes.len()
                && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'_' | b'\''))
            {
                cancel.check()?;
                at += 1;
            }
            let token = &bytes[start..at];
            if matches!(
                token,
                b"fn"
                    | b"fun"
                    | b"let"
                    | b"local"
                    | b"case"
                    | b"handle"
                    | b"val"
                    | b"open"
                    | b"datatype"
                    | b"exception"
                    | b"structure"
                    | b"signature"
                    | b"functor"
                    | b"withtype"
                    | b"infix"
                    | b"infixr"
                    | b"nonfix"
            ) {
                break;
            }
            if token[0].is_ascii_alphabetic() || token[0] == b'_' {
                let start = base
                    .checked_add(u64::try_from(start).map_err(|_| SinkError::InvalidIr)?)
                    .ok_or(SinkError::InvalidIr)?;
                let end = base
                    .checked_add(u64::try_from(at).map_err(|_| SinkError::InvalidIr)?)
                    .ok_or(SinkError::InvalidIr)?;
                let span = SourceSpan::new(file, start, end).map_err(|_| SinkError::InvalidIr)?;
                if let Some(span) = requested(span)? {
                    positions
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    positions.insert(span);
                    last_position = Some(span);
                }
            }
        } else {
            at += 1;
        }
    }
    Ok(positions)
}

fn unique<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|previous| *previous = None)
        .or_insert(Some(value));
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

fn hash(value: &str, cancel: &Cancellation) -> Result<ContentHash, AdapterError> {
    let mut digest = blake3::Hasher::new();
    for chunk in value.as_bytes().chunks(64) {
        cancel.check()?;
        digest.update(chunk);
    }
    Ok(ContentHash::from_bytes(*digest.finalize().as_bytes()))
}

fn whitespace(value: &str, equals: bool, cancel: &Cancellation) -> Result<bool, AdapterError> {
    let mut seen_equals = false;
    for chunk in value.as_bytes().chunks(64) {
        cancel.check()?;
        for byte in chunk {
            if *byte == b'=' && equals && !seen_equals {
                seen_equals = true;
            } else if !byte.is_ascii_whitespace() {
                return Ok(false);
            }
        }
    }
    Ok(seen_equals == equals)
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    *left == super::source_for_span(right, left.span())
}

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "sml.standalone_formal_read.reference"
}

/// A standalone declaration cannot outlive its exact required basis frame.
/// This gate runs before entity drafts, independently of optional read proofs.
pub(super) fn profile_admitted(
    facts: &[SyntaxFact],
    source: &SourceRef,
    mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut ids = std::collections::HashSet::new();
    ids.try_reserve(facts.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    let mut root = None;
    let mut module = None;
    let mut scope = None;
    let mut basis = None;
    let mut signatures = 0_u8;
    let mut invalid = false;
    for fact in facts {
        cancel.check()?;
        invalid |= !ids.insert(fact.local_id()) || fact.kind() == SyntaxFactKind::ErrorRecovery;
        let span = physical(fact.span())?;
        match fact.kind() {
            SyntaxFactKind::Root => {
                invalid |= root.replace(fact).is_some()
                    || fact.syntax_kind().as_str() != "sml.source_file.root"
                    || fact.parent().is_some()
                    || span != source.span();
            }
            SyntaxFactKind::Module => {
                invalid |= module.replace(fact).is_some()
                    || fact.syntax_kind().as_str() != "sml.file.module"
                    || span != source.span();
            }
            SyntaxFactKind::Scope if span == source.span() => {
                invalid |= scope.replace(fact).is_some()
                    || fact.syntax_kind().as_str() != "sml.file.scope";
            }
            SyntaxFactKind::Signature if span == source.span() => {
                signatures = signatures.saturating_add(1).min(2);
                if fact.syntax_kind().as_str() == "sml.closed_standard_basis_context.signature" {
                    invalid |= basis.replace(fact).is_some();
                }
            }
            _ => {}
        }
    }
    Ok(!invalid
        && signatures == 1
        && matches!((root, module, scope, basis), (Some(root), Some(module), Some(scope), Some(basis))
            if module.parent() == Some(root.local_id())
                && scope.parent() == Some(module.local_id())
                && basis.parent() == Some(scope.local_id())))
}

/// Admit written formals only inside one retained, independently clean SML example.
/// This authenticates an existing body frame; it does not construct a source root
/// or grant the host a runtime, incoming-basis, formal-read or call environment.
pub(super) fn embedded_parameters(
    facts: &[SyntaxFact],
    diagnostics: &[rootlight_adapter_sdk::AdapterDiagnostic],
    source: &SourceRef,
    mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
    cancel: &Cancellation,
) -> Result<std::collections::HashSet<u64>, AdapterError> {
    let mut admitted = std::collections::HashSet::new();
    for diagnostic in diagnostics {
        cancel.check()?;
        if diagnostic.code().as_str() == "syntax-extraction-limit" {
            return Ok(admitted);
        }
    }
    let mut by_id = HashMap::new();
    by_id
        .try_reserve(facts.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    let mut root = None;
    for fact in facts {
        cancel.check()?;
        let span = physical(fact.span())?;
        if !embedded_contains(source.span(), span) || by_id.insert(fact.local_id(), fact).is_some()
        {
            return Ok(admitted);
        }
        if fact.kind() == SyntaxFactKind::Root {
            if root.replace(fact).is_some()
                || fact.syntax_kind().as_str() != "markdown.file.root"
                || fact.parent().is_some()
                || span != source.span()
            {
                return Ok(admitted);
            }
        }
    }
    let Some(root) = root else {
        return Ok(admitted);
    };
    for (block, group) in embedded_groups(facts, cancel)? {
        cancel.check()?;
        if !embedded_reaches(block, root.local_id(), &by_id, cancel)? {
            continue;
        }
        let mut module = None;
        let mut invalid = false;
        for &fact in &group {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::Module
                && fact.syntax_kind().as_str() == "sml.file.module"
            {
                invalid |= module.replace(fact).is_some();
            }
        }
        let Some(module) = module.filter(|_| !invalid) else {
            continue;
        };
        let body = module.span();
        if body.start_byte() >= body.end_byte()
            || !embedded_contains(block.span(), body)
            || !embedded_reaches(module, block.local_id(), &by_id, cancel)?
        {
            continue;
        }
        let mut clean = false;
        let mut module_count = 0_u8;
        let mut scope_count = 0_u8;
        let mut signature_count = 0_u8;
        let mut scope = None;
        let mut basis = None;
        for &fact in &group {
            cancel.check()?;
            let label = fact.syntax_kind().as_str();
            if label.starts_with("markdown.")
                && label.contains("code_")
                && label.ends_with(".signature")
                && embedded_overlaps(body, fact.span())
            {
                invalid |= clean
                    || fact.kind() != SyntaxFactKind::Signature
                    || label != "markdown.code_parsed.signature"
                    || fact.span() != block.span()
                    || !embedded_reaches(fact, block.local_id(), &by_id, cancel)?;
                clean = true;
            }
            if fact.span() == body {
                match fact.kind() {
                    SyntaxFactKind::Module => {
                        module_count = module_count.saturating_add(1).min(2);
                    }
                    SyntaxFactKind::Scope => {
                        scope_count = scope_count.saturating_add(1).min(2);
                        if label == "sml.file.scope" {
                            invalid |= scope.replace(fact).is_some();
                        }
                    }
                    SyntaxFactKind::Signature => {
                        signature_count = signature_count.saturating_add(1).min(2);
                        if label == "sml.closed_standard_basis_context.signature" {
                            invalid |= basis.replace(fact).is_some();
                        }
                    }
                    _ => {}
                }
            }
            if !embedded_contains(body, fact.span()) {
                invalid |= label.starts_with("sml.");
                continue;
            }
            if fact.kind() == SyntaxFactKind::Signature
                && label == "markdown.continuation.signature"
                && fact.span().start_byte() == fact.span().end_byte()
                && fact
                    .parent()
                    .and_then(|id| by_id.get(&id))
                    .is_some_and(|owner| {
                        owner.kind() == SyntaxFactKind::Scope
                            && owner.syntax_kind().as_str() == "sml.file.scope"
                            && owner.span() == body
                            && owner.parent() == Some(module.local_id())
                    })
                && embedded_reaches(fact, module.local_id(), &by_id, cancel)?
            {
                // Host line-continuation metadata has no bytes or SML binding authority.
                continue;
            }
            // Other fences may recover; this body's own facts must remain clean,
            // within their native SML frame and beneath the actual host block.
            invalid |= fact.kind() == SyntaxFactKind::ErrorRecovery
                || !label.starts_with("sml.")
                || (fact.local_id() != module.local_id()
                    && !embedded_reaches(fact, module.local_id(), &by_id, cancel)?);
        }
        let frame = matches!((scope, basis), (Some(scope), Some(basis))
            if scope.parent() == Some(module.local_id())
                && basis.parent() == Some(scope.local_id()));
        if invalid
            || !clean
            || !frame
            || module_count != 1
            || scope_count != 1
            || signature_count != 1
        {
            continue;
        }
        for &fact in &group {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == "sml.standalone_parameter.declaration"
                && embedded_contains(body, fact.span())
                && embedded_reaches(fact, module.local_id(), &by_id, cancel)?
            {
                admitted
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                admitted.insert(fact.local_id());
            }
        }
    }
    Ok(admitted)
}

type EmbeddedGroup<'a> = (&'a SyntaxFact, Vec<&'a SyntaxFact>);

fn embedded_groups<'a>(
    facts: &'a [SyntaxFact],
    cancel: &Cancellation,
) -> Result<Vec<EmbeddedGroup<'a>>, AdapterError> {
    let mut blocks = Vec::new();
    for fact in facts {
        cancel.check()?;
        if fact.kind() == SyntaxFactKind::Module
            && fact.syntax_kind().as_str() == "markdown.code_block.module"
        {
            blocks
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            blocks.push(fact);
        }
    }
    crate::runtime::sort_cancellable_by(&mut blocks, cancel, |a, b| {
        a.span().start_byte().cmp(&b.span().start_byte())
    })?;
    for pair in blocks.windows(2) {
        cancel.check()?;
        if pair[0].span().end_byte() > pair[1].span().start_byte() {
            return Ok(Vec::new());
        }
    }
    let mut groups: Vec<EmbeddedGroup<'_>> = Vec::new();
    groups
        .try_reserve_exact(blocks.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    groups.extend(blocks.into_iter().map(|block| (block, Vec::new())));
    for fact in facts {
        cancel.check()?;
        let position = groups
            .partition_point(|(block, _)| block.span().start_byte() <= fact.span().start_byte());
        let selected = position
            .checked_sub(1)
            .and_then(|index| groups.get_mut(index));
        if let Some((block, group)) = selected
            && embedded_contains(block.span(), fact.span())
        {
            group
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            group.push(fact);
        } else if fact.syntax_kind().as_str().starts_with("sml.") {
            return Ok(Vec::new());
        }
    }
    Ok(groups)
}

fn embedded_contains(owner: SourceSpan, child: SourceSpan) -> bool {
    owner.file() == child.file()
        && owner.start_byte() <= child.start_byte()
        && child.end_byte() <= owner.end_byte()
}

fn embedded_overlaps(left: SourceSpan, right: SourceSpan) -> bool {
    left.file() == right.file()
        && left.start_byte() < right.end_byte()
        && right.start_byte() < left.end_byte()
}

fn embedded_reaches<'a>(
    fact: &'a SyntaxFact,
    owner: u64,
    by_id: &HashMap<u64, &'a SyntaxFact>,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut child = fact;
    for _ in 0..by_id.len() {
        cancel.check()?;
        let Some(parent) = child.parent().and_then(|id| by_id.get(&id)).copied() else {
            return Ok(false);
        };
        if parent.depth() >= child.depth() || !embedded_contains(parent.span(), child.span()) {
            return Ok(false);
        }
        if parent.local_id() == owner {
            return Ok(true);
        }
        child = parent;
    }
    Ok(false)
}

pub(super) struct ParameterTargets<'a> {
    source: &'a SourceRef,
    names: HashMap<(SymbolId, ContentHash), Option<(&'a MaterializedEntity, u64)>>,
    markers: HashMap<SourceSpan, Option<&'a SyntaxFact>>,
    signatures: HashMap<SourceSpan, u8>,
    sites: HashMap<SourceSpan, u8>,
    positions: std::collections::HashSet<SourceSpan>,
}

impl<'a> ParameterTargets<'a> {
    pub(super) fn new<'text>(
        facts: &'a [SyntaxFact],
        entities: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut by_id = HashMap::new();
        let mut headers = HashMap::new();
        let mut parameter_headers = HashMap::new();
        let mut markers = HashMap::new();
        for map in [&mut by_id, &mut headers, &mut parameter_headers] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        markers
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut signatures = HashMap::<SourceSpan, u8>::new();
        let mut sites = HashMap::<SourceSpan, u8>::new();
        let mut witnesses = HashMap::<(SourceSpan, &'static str), u8>::new();
        signatures
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        sites
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        witnesses
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut basis = None;
        let mut root = None;
        let mut module = None;
        let mut scope = None;
        let mut invalid = false;
        for fact in facts {
            cancel.check()?;
            if by_id.contains_key(&fact.local_id()) {
                invalid = true;
            }
            unique(&mut by_id, fact.local_id(), fact);
            invalid |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            let span = physical(fact.span())?;
            if fact.kind() == SyntaxFactKind::Root {
                invalid |= root.replace(fact).is_some()
                    || fact.syntax_kind().as_str() != "sml.source_file.root"
                    || fact.parent().is_some()
                    || span != source.span();
            }
            if fact.kind() == SyntaxFactKind::Module {
                invalid |= module.replace(fact).is_some()
                    || fact.syntax_kind().as_str() != "sml.file.module"
                    || span != source.span();
            }
            if fact.kind() == SyntaxFactKind::Scope && span == source.span() {
                invalid |= scope.replace(fact).is_some()
                    || fact.syntax_kind().as_str() != "sml.file.scope";
            }
            if fact.kind() == SyntaxFactKind::Signature {
                signatures
                    .entry(span)
                    .and_modify(|count| *count = 2)
                    .or_insert(1);
                match fact.syntax_kind().as_str() {
                    "sml.closed_standard_basis_context.signature" => {
                        invalid |= basis.replace(fact).is_some() || span != source.span();
                    }
                    "sml.header.signature" => {
                        if let Some(parent) = fact.parent() {
                            unique(&mut headers, parent, fact);
                        }
                    }
                    "sml.formal_header.signature" => {
                        if let Some(parent) = fact.parent() {
                            unique(&mut parameter_headers, parent, fact);
                        }
                    }
                    "sml.standalone_formal_target.signature" => unique(&mut markers, span, fact),
                    _ => {}
                }
            }
            if fact.kind() == SyntaxFactKind::Occurrence {
                sites
                    .entry(span)
                    .and_modify(|count| *count = 2)
                    .or_insert(1);
            }
            let role = if fact.kind() == SyntaxFactKind::Declaration {
                Some("declaration")
            } else if fact.kind() == SyntaxFactKind::Occurrence
                && (fact.syntax_kind().as_str().ends_with(".definition")
                    || fact.syntax_kind().as_str().ends_with(".definition_part"))
            {
                Some("definition")
            } else {
                None
            };
            if let Some(role) = role {
                witnesses
                    .entry((span, role))
                    .and_modify(|count| *count = 2)
                    .or_insert(1);
            }
        }
        let mut names = HashMap::new();
        let mut positions = std::collections::HashSet::new();
        let context = matches!((root, module, scope, basis), (Some(root), Some(module), Some(scope), Some(basis))
            if module.parent() == Some(root.local_id()) && scope.parent() == Some(module.local_id())
                && basis.parent() == Some(scope.local_id()) && signatures.get(&source.span()) == Some(&1));
        if invalid || !context {
            return Ok(Self {
                source,
                names,
                markers,
                signatures,
                sites,
                positions,
            });
        }
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut owners = HashMap::new();
        owners
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            cancel.check()?;
            unique(&mut owners, entity.record.id, (*local, entity));
        }
        for (local, parameter) in entities {
            cancel.check()?;
            if parameter.record.kind != EntityKind::Parameter
                || parameter.record.language != "standard-ml"
            {
                continue;
            }
            let ContainerRef::Entity(owner_id) = parameter.direct_parent else {
                continue;
            };
            let key = (owner_id, hash(&parameter.record.canonical_name, cancel)?);
            if names.insert(key, None).is_some() {
                continue;
            }
            let Some((owner_local, owner)) = owners.get(&owner_id).copied().flatten() else {
                continue;
            };
            let Some(owner_source) = owner.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(parameter_source) = parameter.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(declaration) = by_id.get(local).copied().flatten() else {
                continue;
            };
            let Some(owner_declaration) = by_id.get(&owner_local).copied().flatten() else {
                continue;
            };
            let Some(function_scope) = owner_declaration
                .parent()
                .and_then(|id| by_id.get(&id).copied().flatten())
            else {
                continue;
            };
            let Some(header) = headers.get(&owner_local).copied().flatten() else {
                continue;
            };
            let Some(parameter_header) = parameter_headers.get(local).copied().flatten() else {
                continue;
            };
            let Some(definition) = parameter
                .definition_local_id
                .and_then(|id| by_id.get(&id).copied().flatten())
            else {
                continue;
            };
            let Some(owner_definition) = owner
                .definition_local_id
                .and_then(|id| by_id.get(&id).copied().flatten())
            else {
                continue;
            };
            let owner_span = physical(owner_declaration.span())?;
            let parameter_span = physical(declaration.span())?;
            let header_span = physical(header.span())?;
            if declaration.kind() != SyntaxFactKind::Declaration
                || declaration.syntax_kind().as_str() != "sml.standalone_parameter.declaration"
                || declaration.parent() != Some(owner_local)
                || owner_declaration.kind() != SyntaxFactKind::Declaration
                || owner_declaration.syntax_kind().as_str() != "sml.function.declaration"
                || function_scope.kind() != SyntaxFactKind::Scope
                || function_scope.syntax_kind().as_str() != "sml.function.scope"
                || function_scope.parent() != scope.map(SyntaxFact::local_id)
                || physical(function_scope.span())? != owner_span
                || definition.kind() != SyntaxFactKind::Occurrence
                || definition.syntax_kind().as_str() != "sml.formal_name.definition"
                || definition.parent() != Some(*local)
                || owner_definition.kind() != SyntaxFactKind::Occurrence
                || owner_definition.syntax_kind().as_str() != "sml.name.definition"
                || owner_definition.parent() != Some(owner_local)
                || physical(definition.span())? != parameter_span
                || physical(parameter_header.span())? != parameter_span
                || parameter_header.parent() != Some(*local)
                || header.parent() != Some(owner_local)
                || signatures.get(&header_span) != Some(&1)
                || signatures.get(&parameter_span) != Some(&1)
                || header_span.start_byte() != owner_span.start_byte()
                || !super::span_contains(owner_span, header_span)
                || !super::span_contains(header_span, parameter_span)
                || witnesses.get(&(owner_span, "declaration")) != Some(&1)
                || witnesses.get(&(parameter_span, "declaration")) != Some(&1)
                || witnesses.get(&(physical(owner_definition.span())?, "definition")) != Some(&1)
                || witnesses.get(&(parameter_span, "definition")) != Some(&1)
                || owner.record.kind != EntityKind::Function
                || owner.record.language != "standard-ml"
                || owner.direct_parent != ContainerRef::File(source.span().file())
                || owner.record.container != Some(owner.direct_parent)
                || parameter.record.container != Some(parameter.direct_parent)
                || owner.record.provenance != parameter.record.provenance
                || owner.record.repository != source.repository()
                || parameter.record.repository != source.repository()
                || owner.record.generation != source.generation()
                || parameter.record.generation != source.generation()
                || !same_source(owner_source, source)
                || !same_source(parameter_source, source)
                || owner_source.span() != owner_span
                || parameter_source.span() != parameter_span
                || !equal(
                    text(owner_definition.span())?,
                    &owner.record.canonical_name,
                    cancel,
                )?
                || !equal(
                    text(definition.span())?,
                    &parameter.record.canonical_name,
                    cancel,
                )?
                || owner_definition.span().end_byte() >= declaration.span().start_byte()
                || definition.span().end_byte() > header.span().end_byte()
            {
                continue;
            }
            let between = SourceSpan::new(
                declaration.span().file(),
                owner_definition.span().end_byte(),
                declaration.span().start_byte(),
            )
            .map_err(|_| SinkError::InvalidIr)?;
            let tail = SourceSpan::new(
                declaration.span().file(),
                definition.span().end_byte(),
                header.span().end_byte(),
            )
            .map_err(|_| SinkError::InvalidIr)?;
            if !whitespace(text(between)?, false, cancel)?
                || !whitespace(text(tail)?, true, cancel)?
            {
                continue;
            }
            let body = SourceSpan::new(
                declaration.span().file(),
                header.span().end_byte(),
                owner_declaration.span().end_byte(),
            )
            .map_err(|_| SinkError::InvalidIr)?;
            for span in source_positions(
                text(body)?,
                body.start_byte(),
                body.file(),
                |span| {
                    let span = physical(span)?;
                    Ok((sites.get(&span) == Some(&1)).then_some(span))
                },
                cancel,
            )? {
                cancel.check()?;
                positions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                positions.insert(span);
            }
            names.insert(key, Some((parameter, owner_local)));
        }
        Ok(Self {
            source,
            names,
            markers,
            signatures,
            sites,
            positions,
        })
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        span: SourceSpan,
        site: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        spelling: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(owner) = owner else { return Ok(None) };
        let Some((parameter, owner_local)) = self
            .names
            .get(&(owner.record.id, hash(spelling, cancel)?))
            .copied()
            .flatten()
        else {
            return Ok(None);
        };
        let Some(parameter_source) = parameter.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if !required(fact)
            || site.role != OccurrenceRole::Read
            || site.syntax_kind != "sml.standalone_formal_read.reference"
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
            || !same_source(&site.source, self.source)
            || site.source.span() != span
            || site.enclosing != Some(owner.record.id)
            || site.provenance != parameter.record.provenance
            || site.syntactic_text_hash != hash(spelling, cancel)?
            || fact.parent() != Some(owner_local)
            || !super::span_contains(owner_source.span(), span)
            || parameter_source.span().end_byte() > span.start_byte()
            || self.sites.get(&span) != Some(&1)
            || !self.positions.contains(&span)
            || !equal(spelling, &parameter.record.canonical_name, cancel)?
        {
            return Ok(None);
        }
        let window = SourceSpan::new(
            span.file(),
            parameter_source.span().start_byte(),
            span.end_byte(),
        )
        .map_err(|_| SinkError::InvalidIr)?;
        let Some(marker) = self.markers.get(&window).copied().flatten() else {
            return Ok(None);
        };
        if marker.parent() != fact.parent() || self.signatures.get(&window) != Some(&1) {
            return Ok(None);
        }
        Ok(Some(parameter.record.id))
    }
}
