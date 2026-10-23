//! Join Eiffel receiver certificates to independently parsed current formal owners.
//! A second bounded vendored-grammar pass checks the consumer's trust boundary;
//! feature dispatch and the original unresolved Reference remain unchanged.

use std::{
    collections::{HashMap, HashSet},
    ops::{ControlFlow, Range},
};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};
use tree_sitter::{Node, ParseOptions};

use super::{MaterializedEntity, provider_failure, span_contains};

pub(super) const READ: &str = "eiffel.formal_receiver.read";
const TARGET: &str = "eiffel.formal_receiver_target.signature";
const CLEAN: &str = "eiffel.formal_receiver_clean_file.signature";

/// Index mandatory native receiver endpoints without interpreting their binding.
pub(super) fn read_ends(
    facts: &[SyntaxFact],
    cancel: &Cancellation,
) -> Result<HashMap<(FileId, u64), ()>, AdapterError> {
    cancel.check()?;
    let mut ends = HashMap::new();
    for fact in facts {
        cancel.check()?;
        if fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str() == TARGET {
            ends.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            ends.insert((fact.span().file(), fact.span().end_byte()), ());
        }
    }
    Ok(ends)
}

pub(super) fn reference(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "eiffel.identifier.reference"
}

struct Work {
    remaining: usize,
    exhausted: bool,
}

impl Work {
    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }
}

#[derive(Clone)]
struct WrittenReceiver {
    class: Range<usize>,
    class_name: Range<usize>,
    parameter: Range<usize>,
    feature: Range<usize>,
    feature_name: Range<usize>,
    header: Range<usize>,
    receiver: Range<usize>,
}

#[derive(Clone)]
struct ClassOwner {
    range: Range<usize>,
    name: Range<usize>,
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|old| *old = None)
        .or_insert(Some(value));
}

fn direct<'tree>(
    node: Node<'tree>,
    kind: &str,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut result = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !work.step(cancel)? {
            return Ok(None);
        }
        if child.kind() == kind && result.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(result)
}

fn folded(
    source: &[u8],
    node: Node<'_>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Vec<u8>>, AdapterError> {
    let Some(raw) = source.get(node.byte_range()).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    if node.kind() != "identifier" || raw.first().is_none_or(u8::is_ascii_digit) {
        return Ok(None);
    }
    let mut result = Vec::new();
    result
        .try_reserve_exact(raw.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for chunk in raw.chunks(64) {
        if !work.step(cancel)? {
            return Ok(None);
        }
        for byte in chunk {
            if !byte.is_ascii_alphanumeric() && *byte != b'_' {
                return Ok(None);
            }
            result.push(byte.to_ascii_lowercase());
        }
    }
    Ok(Some(result))
}

fn qualified_receiver(
    node: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    if !work.step(cancel)? || node.kind() != "identifier" {
        return Ok(false);
    }
    let Some(name) = node.parent() else {
        return Ok(false);
    };
    let Some(target) = name.parent() else {
        return Ok(false);
    };
    let Some(call) = target.parent() else {
        return Ok(false);
    };
    if name.kind() != "unqualified_call"
        || name.named_child_count() != 1
        || name.named_child(0) != Some(node)
        || target.kind() != "call"
        || target.named_child_count() != 1
        || target.named_child(0) != Some(name)
        || call.kind() != "call"
        || call.named_child_count() != 2
        || call.named_child(0) != Some(target)
    {
        return Ok(false);
    }
    let Some(feature) = call
        .named_child(1)
        .filter(|feature| feature.kind() == "unqualified_call")
    else {
        return Ok(false);
    };
    let Some(separator) = source.get(target.end_byte()..feature.start_byte()) else {
        return Ok(false);
    };
    let mut punctuation = None;
    for chunk in separator.chunks(64) {
        if !work.step(cancel)? {
            return Ok(false);
        }
        for byte in chunk {
            if !byte.is_ascii_whitespace() && punctuation.replace(*byte).is_some() {
                return Ok(false);
            }
        }
    }
    Ok(punctuation == Some(b'.'))
}

fn written_feature(
    feature: Node<'_>,
    source: &[u8],
    classes: &mut HashMap<usize, Option<ClassOwner>>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Vec<WrittenReceiver>, AdapterError> {
    let mut result = Vec::new();
    let mut ancestor = feature.parent();
    let mut class = None;
    while let Some(node) = ancestor {
        if !work.step(cancel)? {
            return Ok(result);
        }
        if node.kind() == "class_declaration" {
            class = Some(node);
            break;
        }
        ancestor = node.parent();
    }
    let Some(class) = class else {
        return Ok(result);
    };
    let owner = if let Some(owner) = classes.get(&class.id()) {
        owner.clone()
    } else {
        let owner = direct(class, "class_name", work, cancel)?.map(|name| ClassOwner {
            range: class.byte_range(),
            name: name.byte_range(),
        });
        classes
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        classes.insert(class.id(), owner.clone());
        owner
    };
    let Some(owner) = owner else {
        return Ok(result);
    };
    let Some(head) = direct(feature, "new_feature", work, cancel)? else {
        return Ok(result);
    };
    let Some(extended) = direct(head, "extended_feature_name", work, cancel)? else {
        return Ok(result);
    };
    let Some(feature_name) = extended
        .named_child(0)
        .filter(|name| name.kind() == "identifier" && extended.named_child_count() == 1)
    else {
        return Ok(result);
    };
    let Some(formals) = direct(feature, "formal_arguments", work, cancel)? else {
        return Ok(result);
    };
    let Some(routine) = direct(feature, "attribute_or_routine", work, cancel)? else {
        return Ok(result);
    };
    let Some(body) = direct(routine, "feature_body", work, cancel)? else {
        return Ok(result);
    };
    let Some(effective) = direct(body, "effective_routine", work, cancel)? else {
        return Ok(result);
    };
    let Some(internal) = direct(effective, "internal", work, cancel)? else {
        return Ok(result);
    };
    let mut bindings: HashMap<Vec<u8>, Option<(Range<usize>, bool)>> = HashMap::new();
    let mut uses = Vec::new();
    let mut cursor = feature.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(Vec::new());
        }
        let node = cursor.node();
        if node.is_missing()
            || node.is_error()
            || matches!(
                node.kind(),
                "inline_agent" | "iteration" | "object_test" | "separate"
            )
        {
            return Ok(Vec::new());
        }
        if node.kind() == "identifier" {
            if let Some(group) = node
                .parent()
                .filter(|parent| parent.kind() == "entity_declaration_group")
            {
                let parent = group.parent();
                let formal = parent == Some(formals);
                if !formal && parent.is_none_or(|parent| parent.kind() != "local_declarations") {
                    return Ok(Vec::new());
                }
                let Some(name) = folded(source, node, work, cancel)? else {
                    return Ok(Vec::new());
                };
                bindings
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                unique(&mut bindings, name, (node.byte_range(), formal));
            } else if internal.start_byte() <= node.start_byte()
                && node.end_byte() <= internal.end_byte()
                && qualified_receiver(node, source, work, cancel)?
            {
                let Some(name) = folded(source, node, work, cancel)? else {
                    return Ok(Vec::new());
                };
                uses.try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                uses.push((node.byte_range(), name));
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                for binding in bindings.values() {
                    if !work.step(cancel)? || binding.is_none() {
                        return Ok(Vec::new());
                    }
                }
                for (receiver, name) in uses {
                    if !work.step(cancel)? {
                        return Ok(Vec::new());
                    }
                    if let Some(Some((parameter, true))) = bindings.get(&name) {
                        result
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        result.push(WrittenReceiver {
                            class: owner.range.clone(),
                            class_name: owner.name.clone(),
                            parameter: parameter.clone(),
                            feature: feature.byte_range(),
                            feature_name: feature_name.byte_range(),
                            header: feature.start_byte()..routine.start_byte(),
                            receiver,
                        });
                    }
                }
                return Ok(result);
            }
        }
    }
}

fn source_receivers(
    source: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<HashMap<(usize, usize), WrittenReceiver>, AdapterError> {
    cancel.check()?;
    let mut result = HashMap::new();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_eiffel::LANGUAGE_EIFFEL.into())
        .map_err(|_| provider_failure("eiffel-formal-consumer-grammar"))?;
    let mut remaining = maximum;
    let mut exhausted = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if cancel.check().is_err() {
            return ControlFlow::Break(());
        }
        if let Some(next) = remaining.checked_sub(1) {
            remaining = next;
            ControlFlow::Continue(())
        } else {
            exhausted = true;
            ControlFlow::Break(())
        }
    };
    let mut input = |offset: usize, _: tree_sitter::Point| source.get(offset..).unwrap_or_default();
    let tree = parser.parse_with_options(
        &mut input,
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    cancel.check()?;
    let Some(tree) = tree.filter(|_| !exhausted) else {
        return Ok(result);
    };
    let root = tree.root_node();
    if root.kind() != "source_file"
        || root.has_error()
        || root.is_missing()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
    {
        return Ok(result);
    }
    let mut work = Work {
        remaining: maximum,
        exhausted: false,
    };
    let mut classes = HashMap::new();
    let mut cursor = root.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(HashMap::new());
        }
        let node = cursor.node();
        let feature = node.kind() == "feature_declaration";
        if feature {
            for proof in written_feature(node, source, &mut classes, &mut work, cancel)? {
                if !work.step(cancel)? {
                    return Ok(HashMap::new());
                }
                result
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                if result
                    .insert((proof.parameter.start, proof.receiver.end), proof)
                    .is_some()
                {
                    return Ok(HashMap::new());
                }
            }
        }
        if !feature && cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(if work.exhausted {
                    HashMap::new()
                } else {
                    result
                });
            }
        }
    }
}

fn logical(span: SourceSpan) -> Option<(usize, usize)> {
    Some((
        usize::try_from(span.start_byte()).ok()?,
        usize::try_from(span.end_byte()).ok()?,
    ))
}

fn source_for_span(source: &SourceRef, span: SourceSpan) -> SourceRef {
    SourceRef::new(
        source.repository(),
        source.generation(),
        span,
        source.content_hash(),
        None,
    )
}

fn direct_child(child: &SyntaxFact, parent: &SyntaxFact) -> bool {
    child.parent() == Some(parent.local_id())
        && parent.depth().checked_add(1) == Some(child.depth())
        && span_contains(parent.span(), child.span())
}

fn get_fact<'facts>(
    map: &HashMap<SourceSpan, Option<&'facts SyntaxFact>>,
    span: SourceSpan,
) -> Option<&'facts SyntaxFact> {
    map.get(&span).copied().flatten()
}

fn current_entity(
    entity: &MaterializedEntity,
    kind: EntityKind,
    name: &str,
    definition: u64,
    span: SourceSpan,
    source: &SourceRef,
) -> bool {
    entity.record.kind == kind
        && entity.record.language == "eiffel"
        && entity.record.repository == source.repository()
        && entity.record.generation == source.generation()
        && entity.record.canonical_name == name
        && entity.record.evidence.source.as_ref() == Some(&source_for_span(source, span))
        && entity.definition_local_id == Some(definition)
        && entity.record.container == Some(entity.direct_parent)
        && entity.record.id == entity.identity_claim.symbol
        && entity.record.id == entity.identity_claim.derived_symbol()
}

fn reaches_module<'facts>(
    mut child: &'facts SyntaxFact,
    module: &SyntaxFact,
    all: &HashMap<u64, &'facts SyntaxFact>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    for _ in 0..all.len() {
        if !work.step(cancel)? {
            return Ok(false);
        }
        let Some(parent) = child.parent().and_then(|id| all.get(&id)).copied() else {
            return Ok(false);
        };
        if !direct_child(child, parent) {
            return Ok(false);
        }
        if parent.local_id() == module.local_id() {
            return Ok(true);
        }
        if parent.kind() != SyntaxFactKind::Declaration
            || parent.syntax_kind().as_str() != "eiffel.class.declaration"
        {
            return Ok(false);
        }
        child = parent;
    }
    Ok(false)
}

struct Binding {
    owner: SymbolId,
    target: SymbolId,
    span: SourceSpan,
    hash: ContentHash,
    provenance: FactId,
}

pub(super) struct ReadTargets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl ReadTargets {
    /// Authenticate physical bytes and exact native/CST/entity joins for one source.
    /// Missing, duplicate, stale, ambiguous, or foreign witnesses withhold Reads.
    #[expect(
        clippy::too_many_arguments,
        reason = "logical source and physical current-frame projection are independently authenticated"
    )]
    pub(super) fn new(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        current: &[u8],
        physical_bytes: &[u8],
        maximum: usize,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut result = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        if source.span().start_byte() != 0
            || u64::try_from(physical_bytes.len()).ok() != Some(source.span().end_byte())
        {
            return Ok(result);
        }
        let mut work = Work {
            remaining: maximum,
            exhausted: false,
        };
        let mut has_certificate = false;
        for fact in facts {
            if !work.step(cancel)? {
                return Ok(result);
            }
            if fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str() == TARGET {
                has_certificate = true;
                break;
            }
        }
        if !has_certificate {
            return Ok(result);
        }
        let mut hasher = blake3::Hasher::new();
        for chunk in physical_bytes.chunks(64) {
            if !work.step(cancel)? {
                return Ok(result);
            }
            hasher.update(chunk);
        }
        if ContentHash::from_bytes(*hasher.finalize().as_bytes()) != source.content_hash() {
            return Ok(result);
        }
        let proofs = source_receivers(current, maximum, cancel)?;
        let mut all = HashMap::new();
        let mut unique_facts = HashSet::new();
        let mut declarations = HashMap::new();
        let mut definitions = HashMap::new();
        let mut headers = HashMap::new();
        let mut references = HashMap::new();
        let mut certificates = HashMap::new();
        for map in [
            &mut declarations,
            &mut definitions,
            &mut headers,
            &mut references,
            &mut certificates,
        ] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        all.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        unique_facts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut root = None;
        let mut module = None;
        let mut clean = None;
        let mut invalid = false;
        for fact in facts {
            if !work.step(cancel)? {
                return Ok(result);
            }
            invalid |= all.insert(fact.local_id(), fact).is_some()
                || !unique_facts.insert((fact.kind(), fact.syntax_kind().as_str(), fact.span()))
                || fact.kind() == SyntaxFactKind::ErrorRecovery
                || fact.span().file() != source.span().file()
                || (matches!(fact.syntax_kind().as_str(), TARGET | CLEAN)
                    && fact.kind() != SyntaxFactKind::Signature);
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Root, "eiffel.file.root") => {
                    invalid |= root.replace(fact).is_some()
                        || fact.parent().is_some()
                        || fact.depth() != 0
                        || physical(fact.span())? != source.span();
                }
                (SyntaxFactKind::Module, "eiffel.file.module") => {
                    invalid |= module.replace(fact).is_some();
                }
                (SyntaxFactKind::Signature, CLEAN) => {
                    invalid |= clean.replace(fact).is_some();
                }
                (SyntaxFactKind::Declaration, _) => {
                    unique(&mut declarations, fact.span(), fact);
                }
                (SyntaxFactKind::Occurrence, "eiffel.name.definition") => {
                    unique(&mut definitions, fact.span(), fact);
                }
                (SyntaxFactKind::Signature, "eiffel.written_header.signature") => {
                    unique(&mut headers, fact.span(), fact);
                }
                (SyntaxFactKind::Occurrence, "eiffel.identifier.reference") => {
                    unique(&mut references, fact.span(), fact);
                }
                (SyntaxFactKind::Signature, TARGET) => {
                    unique(&mut certificates, fact.span(), fact);
                }
                _ => {}
            }
        }
        let (Some(root), Some(module), Some(clean)) = (root, module, clean) else {
            return Ok(result);
        };
        if invalid
            || module.span() != root.span()
            || !direct_child(module, root)
            || clean.span() != root.span()
            || !reaches_module(clean, module, &all, &mut work, cancel)?
        {
            return Ok(result);
        }
        let Some(module_entity) = entities.get(&module.local_id()).filter(|entity| {
            entity.record.kind == EntityKind::Module
                && entity.record.language == "eiffel"
                && entity.record.repository == source.repository()
                && entity.record.generation == source.generation()
                && entity.record.evidence.source.as_ref() == Some(source)
                && entity.direct_parent == ContainerRef::File(source.span().file())
                && entity.record.container == Some(entity.direct_parent)
                && entity.record.id == entity.identity_claim.symbol
                && entity.record.id == entity.identity_claim.derived_symbol()
        }) else {
            return Ok(result);
        };
        let mut symbols = HashMap::new();
        symbols
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            if !work.step(cancel)? {
                return Ok(result);
            }
            unique(&mut symbols, entity.record.id, *local);
        }
        for certificate in certificates.values().copied().flatten() {
            if !work.step(cancel)? {
                return Ok(Self {
                    source: source.clone(),
                    bindings: HashMap::new(),
                });
            }
            let Some((target_start, read_end)) = logical(certificate.span()) else {
                continue;
            };
            let Some(proof) = proofs.get(&(target_start, read_end)) else {
                continue;
            };
            let make_span = |range: &Range<usize>| {
                SourceSpan::new(
                    certificate.span().file(),
                    u64::try_from(range.start).ok()?,
                    u64::try_from(range.end).ok()?,
                )
                .ok()
            };
            let (
                Some(class_span),
                Some(class_name_span),
                Some(parameter_span),
                Some(function_span),
                Some(function_name_span),
                Some(header_span),
                Some(read_span),
            ) = (
                make_span(&proof.class),
                make_span(&proof.class_name),
                make_span(&proof.parameter),
                make_span(&proof.feature),
                make_span(&proof.feature_name),
                make_span(&proof.header),
                make_span(&proof.receiver),
            )
            else {
                continue;
            };
            let (
                Some(class),
                Some(class_definition),
                Some(parameter),
                Some(function),
                Some(parameter_definition),
                Some(function_definition),
                Some(header),
                Some(reference),
            ) = (
                get_fact(&declarations, class_span),
                get_fact(&definitions, class_name_span),
                get_fact(&declarations, parameter_span),
                get_fact(&declarations, function_span),
                get_fact(&definitions, parameter_span),
                get_fact(&definitions, function_name_span),
                get_fact(&headers, header_span),
                get_fact(&references, read_span),
            )
            else {
                continue;
            };
            let (Some(class_entity), Some(parameter_entity), Some(function_entity)) = (
                entities.get(&class.local_id()),
                entities.get(&parameter.local_id()),
                entities.get(&function.local_id()),
            ) else {
                continue;
            };
            let Some(written_class) = current
                .get(proof.class_name.clone())
                .and_then(|raw| std::str::from_utf8(raw).ok())
            else {
                continue;
            };
            let Some(written_parameter) = current
                .get(proof.parameter.clone())
                .and_then(|raw| std::str::from_utf8(raw).ok())
            else {
                continue;
            };
            let Some(written_function) = current
                .get(proof.feature_name.clone())
                .and_then(|raw| std::str::from_utf8(raw).ok())
            else {
                continue;
            };
            let parameter_physical = physical(parameter_span)?;
            let function_physical = physical(function_span)?;
            if class.syntax_kind().as_str() != "eiffel.class.declaration"
                || !direct_child(class, module)
                || !direct_child(class_definition, class)
                || !direct_child(function, class)
                || parameter.syntax_kind().as_str() != "eiffel.parameter.declaration"
                || function.syntax_kind().as_str() != "eiffel.routine.declaration"
                || !direct_child(parameter, function)
                || !direct_child(parameter_definition, parameter)
                || !direct_child(function_definition, function)
                || !direct_child(header, function)
                || !direct_child(reference, function)
                || !direct_child(certificate, function)
                || !reaches_module(function, module, &all, &mut work, cancel)?
                || !current_entity(
                    class_entity,
                    EntityKind::Class,
                    written_class,
                    class_definition.local_id(),
                    physical(class_span)?,
                    source,
                )
                || !current_entity(
                    parameter_entity,
                    EntityKind::Parameter,
                    written_parameter,
                    parameter_definition.local_id(),
                    parameter_physical,
                    source,
                )
                || !current_entity(
                    function_entity,
                    EntityKind::Function,
                    written_function,
                    function_definition.local_id(),
                    function_physical,
                    source,
                )
                || class_entity.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || function_entity.direct_parent != ContainerRef::Entity(class_entity.record.id)
                || parameter_entity.direct_parent != ContainerRef::Entity(function_entity.record.id)
                || symbols.get(&class_entity.record.id).copied().flatten() != Some(class.local_id())
                || symbols.get(&parameter_entity.record.id).copied().flatten()
                    != Some(parameter.local_id())
                || symbols.get(&function_entity.record.id).copied().flatten()
                    != Some(function.local_id())
                || parameter_entity.record.provenance != function_entity.record.provenance
                || function_entity.record.provenance != class_entity.record.provenance
                || class_entity.record.provenance != module_entity.record.provenance
                || symbols.get(&module_entity.record.id).copied().flatten()
                    != Some(module.local_id())
            {
                continue;
            }
            let Some(written_read) = current.get(proof.receiver.clone()) else {
                continue;
            };
            result
                .bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.bindings.insert(
                reference.local_id(),
                Binding {
                    owner: function_entity.record.id,
                    target: parameter_entity.record.id,
                    span: physical(read_span)?,
                    hash: content_hash(written_read),
                    provenance: parameter_entity.record.provenance,
                },
            );
        }
        if work.exhausted {
            result.bindings.clear();
        }
        Ok(result)
    }

    /// Select only the certified target for an unchanged unresolved Reference.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if !reference(fact)
            || site.role != OccurrenceRole::Reference
            || site.syntax_kind != "eiffel.identifier.reference"
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
            || site.source.repository() != self.source.repository()
            || site.source.generation() != self.source.generation()
            || site.source.content_hash() != self.source.content_hash()
            || site.evidence.source.as_ref() != Some(&site.source)
        {
            return Ok(None);
        }
        Ok(self
            .bindings
            .get(&fact.local_id())
            .filter(|binding| {
                binding.span == physical
                    && site.source.span() == binding.span
                    && site.enclosing == Some(binding.owner)
                    && site.syntactic_text_hash == binding.hash
                    && site.provenance == binding.provenance
            })
            .map(|binding| binding.target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{GenerationId, RepositoryId};
    use rootlight_ir::{
        AnalysisTier, EntityRecord, EntityVisibility, FactEvidence, SymbolIdentityClaim,
    };

    const TEXT: &str = "class C feature run (value: ANY) do value.use end end\n";

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
        depth: usize,
        kind: SyntaxFactKind,
        range: SourceSpan,
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            range,
            depth,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn witnesses() -> Vec<SyntaxFact> {
        use SyntaxFactKind::{Declaration, Module, Occurrence, Root, Signature};
        let function = TEXT.find("run").unwrap();
        let formal = TEXT.find("value").unwrap();
        let read = TEXT.rfind("value").unwrap();
        vec![
            fact(1, None, 0, Root, span(0, TEXT.len()), "eiffel.file.root"),
            fact(
                2,
                Some(1),
                1,
                Module,
                span(0, TEXT.len()),
                "eiffel.file.module",
            ),
            fact(
                3,
                Some(2),
                2,
                Declaration,
                span(0, TEXT.len() - 1),
                "eiffel.class.declaration",
            ),
            fact(
                4,
                Some(3),
                3,
                Occurrence,
                span(6, 7),
                "eiffel.name.definition",
            ),
            fact(
                5,
                Some(3),
                3,
                Signature,
                span(0, 7),
                "eiffel.written_header.signature",
            ),
            fact(
                6,
                Some(3),
                3,
                Declaration,
                span(function, TEXT.find(" end").unwrap() + 4),
                "eiffel.routine.declaration",
            ),
            fact(
                7,
                Some(6),
                4,
                Occurrence,
                span(function, function + 3),
                "eiffel.name.definition",
            ),
            fact(
                8,
                Some(6),
                4,
                Signature,
                span(function, TEXT.find("do").unwrap()),
                "eiffel.written_header.signature",
            ),
            fact(
                9,
                Some(6),
                4,
                Declaration,
                span(formal, formal + 5),
                "eiffel.parameter.declaration",
            ),
            fact(
                10,
                Some(9),
                5,
                Occurrence,
                span(formal, formal + 5),
                "eiffel.name.definition",
            ),
            fact(
                11,
                Some(9),
                5,
                Signature,
                span(formal, formal + 5),
                "eiffel.written_parameter.signature",
            ),
            fact(
                12,
                Some(6),
                4,
                Occurrence,
                span(read, read + 5),
                "eiffel.identifier.reference",
            ),
            fact(13, Some(6), 4, Signature, span(formal, read + 5), TARGET),
            fact(14, Some(2), 2, Signature, span(0, TEXT.len()), CLEAN),
        ]
    }

    fn entity(
        local: u64,
        definition: u64,
        kind: EntityKind,
        name: &str,
        parent: ContainerRef,
        evidence: SourceSpan,
    ) -> MaterializedEntity {
        let frame = source();
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: frame.repository(),
            language: "eiffel".to_owned(),
            kind,
            container: Some(parent),
            container_identity: format!("written-owner-{local}").into_bytes(),
            declared_identity: name.to_owned(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: Vec::new(),
        };
        claim.symbol = claim.derived_symbol();
        let record = EntityRecord {
            id: claim.symbol,
            repository: frame.repository(),
            generation: frame.generation(),
            kind,
            language: "eiffel".to_owned(),
            tier: AnalysisTier::TierD,
            canonical_name: name.to_owned(),
            display_name: name.to_owned(),
            qualified_name: name.to_owned(),
            container: Some(parent),
            visibility: EntityVisibility::Unknown,
            flags: Vec::new(),
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: Some(source_for_span(&frame, evidence)),
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
        let mut module = entity(
            2,
            0,
            EntityKind::Module,
            "written.e",
            ContainerRef::File(source().span().file()),
            source().span(),
        );
        module.definition_local_id = None;
        let class = entity(
            3,
            4,
            EntityKind::Class,
            "C",
            ContainerRef::Entity(module.record.id),
            facts[2].span(),
        );
        let function = entity(
            6,
            7,
            EntityKind::Function,
            "run",
            ContainerRef::Entity(class.record.id),
            facts[5].span(),
        );
        let parameter = entity(
            9,
            10,
            EntityKind::Parameter,
            "value",
            ContainerRef::Entity(function.record.id),
            facts[8].span(),
        );
        HashMap::from([(2, module), (3, class), (6, function), (9, parameter)])
    }

    fn plan(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        current: &[u8],
        physical: &[u8],
        frame: &SourceRef,
    ) -> ReadTargets {
        ReadTargets::new(
            facts,
            entities,
            frame,
            current,
            physical,
            16_384,
            Ok,
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn eiffel_formal_read_requires_each_exact_current_owner_and_certificate() {
        let original = witnesses();
        let entities = entities();
        assert_eq!(
            plan(
                &original,
                &entities,
                TEXT.as_bytes(),
                TEXT.as_bytes(),
                &source()
            )
            .bindings
            .len(),
            1
        );
        for missing in [0, 1, 2, 3, 5, 6, 7, 8, 9, 11, 12, 13] {
            let mut facts = original.clone();
            facts.remove(missing);
            assert!(
                plan(
                    &facts,
                    &entities,
                    TEXT.as_bytes(),
                    TEXT.as_bytes(),
                    &source()
                )
                .bindings
                .is_empty(),
                "missing={missing}"
            );
        }
        for duplicate in [0, 1, 5, 6, 7, 8, 9, 11, 12, 13] {
            let prior = &original[duplicate];
            let mut facts = original.clone();
            facts.push(fact(
                99,
                prior.parent(),
                prior.depth(),
                prior.kind(),
                prior.span(),
                prior.syntax_kind().as_str(),
            ));
            assert!(
                plan(
                    &facts,
                    &entities,
                    TEXT.as_bytes(),
                    TEXT.as_bytes(),
                    &source()
                )
                .bindings
                .is_empty(),
                "duplicate={duplicate}"
            );
        }
    }

    #[test]
    fn eiffel_formal_read_refuses_ancestor_reparenting_span_changes_and_metadata_counterfeits() {
        let original = witnesses();
        for changed in [5, 6, 7, 8, 9, 11, 12, 13] {
            for mutation in 0..4 {
                let prior = &original[changed];
                let mut facts = original.clone();
                let (parent, depth, kind, range) = match mutation {
                    0 => (
                        if prior.parent() == Some(2) {
                            Some(1)
                        } else {
                            Some(2)
                        },
                        prior.depth(),
                        prior.kind(),
                        prior.span(),
                    ),
                    1 => (
                        prior.parent(),
                        prior.depth() + 1,
                        prior.kind(),
                        prior.span(),
                    ),
                    2 => (
                        prior.parent(),
                        prior.depth(),
                        prior.kind(),
                        if prior.span() == span(0, TEXT.len()) {
                            span(0, TEXT.len() - 1)
                        } else {
                            span(0, TEXT.len())
                        },
                    ),
                    _ => (
                        prior.parent(),
                        prior.depth(),
                        SyntaxFactKind::Scope,
                        prior.span(),
                    ),
                };
                assert_ne!(
                    (parent, depth, kind, range),
                    (prior.parent(), prior.depth(), prior.kind(), prior.span()),
                    "mutation must alter the original witness"
                );
                facts[changed] = fact(
                    prior.local_id(),
                    parent,
                    depth,
                    kind,
                    range,
                    prior.syntax_kind().as_str(),
                );
                assert!(
                    plan(
                        &facts,
                        &entities(),
                        TEXT.as_bytes(),
                        TEXT.as_bytes(),
                        &source()
                    )
                    .bindings
                    .is_empty(),
                    "changed={changed}, mutation={mutation}"
                );
            }
        }
    }

    #[test]
    fn eiffel_formal_read_refuses_stale_source_and_foreign_parameter_or_function_records() {
        let facts = witnesses();
        for local in [6, 9] {
            for mutation in 0..7 {
                let mut changed = entities();
                let entity = changed.get_mut(&local).unwrap();
                match mutation {
                    0 => entity.record.kind = EntityKind::Variable,
                    1 => entity.record.language = "other".to_owned(),
                    2 => entity.definition_local_id = Some(12),
                    3 => entity.record.generation = GenerationId::from_bytes([9; 20]),
                    4 => entity.record.canonical_name = "other".to_owned(),
                    5 => entity.record.container = None,
                    _ => {
                        entity.record.evidence.source = Some(SourceRef::new(
                            source().repository(),
                            source().generation(),
                            entity.record.evidence.source.as_ref().unwrap().span(),
                            content_hash(b"stale"),
                            None,
                        ))
                    }
                }
                assert!(
                    plan(
                        &facts,
                        &changed,
                        TEXT.as_bytes(),
                        TEXT.as_bytes(),
                        &source()
                    )
                    .bindings
                    .is_empty(),
                    "local={local}, mutation={mutation}"
                );
            }
        }
        let mut mutated = TEXT.as_bytes().to_vec();
        mutated[TEXT.find("do").unwrap()] = b'!';
        let changed_source = SourceRef::new(
            source().repository(),
            source().generation(),
            source().span(),
            content_hash(&mutated),
            None,
        );
        assert!(
            plan(&facts, &entities(), &mutated, &mutated, &changed_source)
                .bindings
                .is_empty()
        );
        assert!(
            plan(&facts, &entities(), TEXT.as_bytes(), &mutated, &source())
                .bindings
                .is_empty()
        );
        let stale = SourceRef::new(
            source().repository(),
            GenerationId::from_bytes([9; 20]),
            source().span(),
            source().content_hash(),
            None,
        );
        assert!(
            plan(
                &facts,
                &entities(),
                TEXT.as_bytes(),
                TEXT.as_bytes(),
                &stale
            )
            .bindings
            .is_empty()
        );
    }

    #[test]
    fn consumer_reparses_current_binders_instead_of_trusting_old_clean_metadata() {
        let facts = witnesses();
        let old = TEXT.as_bytes();
        let changed = TEXT.replace(" do ", " !! ");
        assert_eq!(old.len(), changed.len());
        assert!(
            plan(&facts, &entities(), changed.as_bytes(), old, &source())
                .bindings
                .is_empty()
        );
        let aliases = "class C feature run (value, VALUE: ANY) do value.use end end\n";
        assert!(
            source_receivers(aliases.as_bytes(), 16_384, &Cancellation::new())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn eiffel_read_resolves_only_its_original_unresolved_current_reference() {
        let facts = witnesses();
        let entities = entities();
        let targets = plan(
            &facts,
            &entities,
            TEXT.as_bytes(),
            TEXT.as_bytes(),
            &source(),
        );
        let read = &facts[11];
        let site = super::super::unresolved_occurrence(
            read,
            OccurrenceRole::Reference,
            Some(entities[&6].record.id),
            entities[&9].record.provenance,
            rootlight_ir::Confidence::new(500).unwrap(),
            source_for_span(&source(), read.span()),
            "value",
        )
        .unwrap();
        assert_eq!(
            targets
                .resolve(read, read.span(), &site, &Cancellation::new())
                .unwrap(),
            Some(entities[&9].record.id)
        );
        for mutation in 0..7 {
            let mut changed = site.clone();
            match mutation {
                0 => changed.role = OccurrenceRole::Read,
                1 => changed.syntactic_text_hash = content_hash(b"other"),
                2 => changed.provenance = FactId::from_bytes([9; 20]),
                3 => changed.enclosing = Some(entities[&3].record.id),
                4 => changed.generation = GenerationId::from_bytes([9; 20]),
                5 => {
                    changed.target = OccurrenceTarget::Resolved {
                        symbol: entities[&9].record.id,
                    }
                }
                _ => changed.evidence.source = None,
            }
            assert_eq!(
                targets
                    .resolve(read, read.span(), &changed, &Cancellation::new())
                    .unwrap(),
                None,
                "mutation={mutation}"
            );
        }
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            targets.resolve(read, read.span(), &site, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            ReadTargets::new(
                &facts,
                &entities,
                &source(),
                TEXT.as_bytes(),
                TEXT.as_bytes(),
                16_384,
                Ok,
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn eiffel_consumer_work_refusal_never_leaves_partial_read_bindings() {
        let facts = witnesses();
        for maximum in 0..16 {
            let targets = ReadTargets::new(
                &facts,
                &entities(),
                &source(),
                TEXT.as_bytes(),
                TEXT.as_bytes(),
                maximum,
                Ok,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(targets.bindings.is_empty(), "maximum={maximum}");
        }
    }

    #[test]
    fn eiffel_consumer_source_census_refusal_drops_earlier_feature_results() {
        let source = b"class C feature first (value: ANY) do value.use end second (value: ANY) do value.use end end\n";
        assert_eq!(
            source_receivers(source, 16_384, &Cancellation::new())
                .unwrap()
                .len(),
            2
        );
        for maximum in 0..512 {
            let found = source_receivers(source, maximum, &Cancellation::new())
                .unwrap()
                .len();
            assert!(matches!(found, 0 | 2), "maximum={maximum}, found={found}");
        }
    }

    #[test]
    fn eiffel_consumer_keeps_logical_endpoints_and_physical_utf16_evidence_separate() {
        let facts = witnesses();
        let mut entities = entities();
        let raw = TEXT
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let project = |span: SourceSpan| {
            SourceSpan::new(
                span.file(),
                span.start_byte().checked_mul(2).unwrap(),
                span.end_byte().checked_mul(2).unwrap(),
            )
            .map_err(|_| AdapterError::from(SinkError::SourceMismatch))
        };
        let frame = SourceRef::new(
            source().repository(),
            source().generation(),
            project(source().span()).unwrap(),
            content_hash(&raw),
            None,
        );
        for entity in entities.values_mut() {
            let span = entity.record.evidence.source.as_ref().unwrap().span();
            entity.record.evidence.source = Some(source_for_span(&frame, project(span).unwrap()));
        }
        let targets = ReadTargets::new(
            &facts,
            &entities,
            &frame,
            TEXT.as_bytes(),
            &raw,
            16_384,
            project,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(targets.bindings.len(), 1);
        let reference = &facts[11];
        let physical = project(reference.span()).unwrap();
        let site = super::super::unresolved_occurrence(
            reference,
            OccurrenceRole::Reference,
            Some(entities[&6].record.id),
            entities[&9].record.provenance,
            rootlight_ir::Confidence::new(500).unwrap(),
            source_for_span(&frame, physical),
            "value",
        )
        .unwrap();
        assert_eq!(
            targets
                .resolve(reference, physical, &site, &Cancellation::new())
                .unwrap(),
            Some(entities[&9].record.id)
        );
        assert_eq!(
            targets
                .resolve(reference, reference.span(), &site, &Cancellation::new())
                .unwrap(),
            None
        );
    }
}
