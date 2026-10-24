//! Independently validate CUDA certificates against current text and raw facts.
//! Native certificates nominate endpoints only. This bounded second parse joins
//! exact written owners, definitions and physical provenance before adding a facet.

use std::{
    cell::Cell,
    collections::HashMap,
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

use super::{MaterializedEntity, provider_failure};

#[cfg(test)]
mod proof_tests;

pub(super) const READ: &str = "cuda.scalar_formal.read";
pub(super) const CALL: &str = "cuda.kernel_launch.call";
const READ_TARGET: &str = "cuda.scalar_read_target.signature";
const CALL_TARGET: &str = "cuda.kernel_launch_target.signature";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Facet {
    Read,
    Call,
}

/// Bound independent accounting by the caller's admitted native-node quota.
pub(super) fn accounting_limit(nodes: usize) -> Result<usize, AdapterError> {
    nodes
        .min(200_000)
        .checked_mul(64)
        .ok_or_else(|| provider_failure("cuda-accounting-work-limit"))
}

pub(super) struct Endpoints {
    entries: Vec<(FileId, u64, Facet, Option<u64>)>,
    maximum_starts: Vec<Option<u64>>,
    leaves: usize,
    remaining: Cell<usize>,
}

impl Endpoints {
    fn step(&self, cancel: &Cancellation) -> Result<(), AdapterError> {
        cancel.check()?;
        self.remaining.set(
            self.remaining
                .get()
                .checked_sub(1)
                .ok_or_else(|| provider_failure("cuda-accounting-work-limit"))?,
        );
        Ok(())
    }

    fn bound(
        &self,
        key: &(FileId, u64),
        after_equal: bool,
        cancel: &Cancellation,
    ) -> Result<usize, AdapterError> {
        let (mut start, mut end) = (0, self.entries.len());
        while start < end {
            self.step(cancel)?;
            let middle = start + (end - start) / 2;
            let value = (self.entries[middle].0, self.entries[middle].1);
            if value < *key || (after_equal && value == *key) {
                start = middle + 1;
            } else {
                end = middle;
            }
        }
        Ok(start)
    }

    pub(super) fn get(
        &self,
        key: &(FileId, u64),
        cancel: &Cancellation,
    ) -> Result<Option<&Facet>, AdapterError> {
        let index = self.bound(key, false, cancel)?;
        self.step(cancel)?;
        Ok(self
            .entries
            .get(index)
            .filter(|entry| (entry.0, entry.1) == *key)
            .map(|entry| &entry.2))
    }

    pub(super) fn contains_key(
        &self,
        key: &(FileId, u64),
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        Ok(self.get(key, cancel)?.is_some())
    }

    /// Query full identifier containment, matching the native scope producer.
    pub(super) fn scope_required(
        &self,
        fact: &SyntaxFact,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if fact.kind() != SyntaxFactKind::Scope
            || !matches!(
                fact.syntax_kind().as_str(),
                "cpp.block.scope" | "cuda.block.scope"
            )
        {
            return Ok(false);
        }
        let span = fact.span();
        let mut start = self.bound(&(span.file(), span.start_byte()), true, cancel)? + self.leaves;
        let mut end = self.bound(&(span.file(), span.end_byte()), true, cancel)? + self.leaves;
        let mut maximum = None;
        while start < end {
            self.step(cancel)?;
            if !start.is_multiple_of(2) {
                maximum = maximum.max(self.maximum_starts[start]);
                start += 1;
            }
            if !end.is_multiple_of(2) {
                end -= 1;
                maximum = maximum.max(self.maximum_starts[end]);
            }
            start /= 2;
            end /= 2;
        }
        Ok(maximum.is_some_and(|start| start >= span.start_byte()))
    }
}

/// Index marker ends and Reference starts with charged cancellable merge sorting.
pub(super) fn ends(
    facts: &[SyntaxFact],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<Endpoints, AdapterError> {
    let mut result = Endpoints {
        entries: Vec::new(),
        maximum_starts: Vec::new(),
        leaves: 1,
        remaining: Cell::new(maximum),
    };
    for fact in facts {
        result.step(cancel)?;
        let facet = match (fact.kind(), fact.syntax_kind().as_str()) {
            (SyntaxFactKind::Signature, READ_TARGET) => Facet::Read,
            (SyntaxFactKind::Signature, CALL_TARGET) => Facet::Call,
            _ => continue,
        };
        result
            .entries
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .entries
            .push((fact.span().file(), fact.span().end_byte(), facet, None));
    }
    let mut scratch = Vec::new();
    scratch
        .try_reserve_exact(result.entries.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for entry in &result.entries {
        result.step(cancel)?;
        scratch.push(*entry);
    }
    let mut width = 1;
    while width < result.entries.len() {
        let mut left = 0;
        while left < result.entries.len() {
            let middle = left.saturating_add(width).min(result.entries.len());
            let end = middle.saturating_add(width).min(result.entries.len());
            let (mut first, mut second) = (left, middle);
            for destination in &mut scratch[left..end] {
                result.step(cancel)?;
                let take_first = if first == middle {
                    false
                } else if second == end {
                    true
                } else {
                    result.step(cancel)?;
                    (result.entries[first].0, result.entries[first].1)
                        <= (result.entries[second].0, result.entries[second].1)
                };
                let selected = if take_first {
                    let selected = first;
                    first += 1;
                    selected
                } else {
                    let selected = second;
                    second += 1;
                    selected
                };
                *destination = result.entries[selected];
            }
            left = end;
        }
        std::mem::swap(&mut result.entries, &mut scratch);
        width = width
            .checked_mul(2)
            .ok_or_else(|| provider_failure("cuda-accounting-work-limit"))?;
    }
    let mut retained = 0;
    for index in 0..result.entries.len() {
        result.step(cancel)?;
        let value = result.entries[index];
        if retained > 0
            && (
                result.entries[retained - 1].0,
                result.entries[retained - 1].1,
            ) == (value.0, value.1)
        {
            result.entries[retained - 1] = value;
        } else {
            result.entries[retained] = value;
            retained += 1;
        }
    }
    result.entries.truncate(retained);
    for fact in facts {
        result.step(cancel)?;
        if reference(fact) {
            let key = (fact.span().file(), fact.span().end_byte());
            let index = result.bound(&key, false, cancel)?;
            if let Some(entry) = result
                .entries
                .get_mut(index)
                .filter(|entry| (entry.0, entry.1) == key)
            {
                entry.3 = entry.3.max(Some(fact.span().start_byte()));
            }
        }
    }
    result.leaves = result
        .entries
        .len()
        .max(1)
        .checked_next_power_of_two()
        .ok_or_else(|| provider_failure("cuda-accounting-work-limit"))?;
    let slots = result
        .leaves
        .checked_mul(2)
        .ok_or_else(|| provider_failure("cuda-accounting-work-limit"))?;
    result
        .maximum_starts
        .try_reserve_exact(slots)
        .map_err(|_| SinkError::AllocationFailed)?;
    for _ in 0..slots {
        result.step(cancel)?;
        result.maximum_starts.push(None);
    }
    for (index, entry) in result.entries.iter().enumerate() {
        result.step(cancel)?;
        result.maximum_starts[result.leaves + index] = entry.3;
    }
    for index in (1..result.leaves).rev() {
        result.step(cancel)?;
        result.maximum_starts[index] =
            result.maximum_starts[index * 2].max(result.maximum_starts[index * 2 + 1]);
    }
    Ok(result)
}

pub(super) fn reference(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "cpp.identifier.reference" | "cuda.identifier.reference"
        )
}

pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            READ_TARGET
                | CALL_TARGET
                | "cuda.clean_file.signature"
                | "cuda.owned_function.signature"
        )
}

struct Work {
    remaining: usize,
    depth_limit: usize,
}
impl Work {
    fn step(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        cancel.check()?;
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| provider_failure("cuda-consumer-work-limit"))?;
        Ok(())
    }
}

struct Written {
    facet: Facet,
    site: Range<usize>,
    owner: Range<usize>,
    owner_name: Range<usize>,
    header: Range<usize>,
    target: Range<usize>,
    target_name: Range<usize>,
    ancestors: Vec<(Range<usize>, bool)>,
}

fn written_ancestors(
    site: Node<'_>,
    owner: Node<'_>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Vec<(Range<usize>, bool)>, AdapterError> {
    let mut result = Vec::new();
    let mut parent = site.parent();
    while let Some(node) = parent {
        work.step(cancel)?;
        if node == owner {
            break;
        }
        if matches!(node.kind(), "compound_statement" | "declaration") {
            result
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.push((node.byte_range(), node.kind() == "compound_statement"));
        }
        parent = node.parent();
    }
    Ok(result)
}

fn collect<'tree>(
    root: Node<'tree>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Vec<Node<'tree>>, AdapterError> {
    let mut result = Vec::new();
    let mut cursor = root.walk();
    let mut depth = 0_usize;
    loop {
        work.step(cancel)?;
        let node = cursor.node();
        result
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        result.push(node);
        if cursor.goto_first_child() {
            depth = depth
                .checked_add(1)
                .ok_or_else(|| provider_failure("cuda-consumer-syntax-limit"))?;
            if depth > work.depth_limit {
                return Err(provider_failure("cuda-consumer-syntax-limit"));
            }
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(result);
            }
            depth = depth.saturating_sub(1);
        }
    }
}

fn identifier<'source>(
    node: Node<'_>,
    source: &'source [u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<&'source str>, AdapterError> {
    work.step(cancel)?;
    let Some(raw) = source.get(node.byte_range()) else {
        return Ok(None);
    };
    for _ in raw.chunks(64) {
        work.step(cancel)?;
    }
    let Some((first, rest)) = raw.split_first() else {
        return Ok(None);
    };
    Ok((node.kind() == "identifier"
        && (*first == b'_' || first.is_ascii_alphabetic())
        && rest
            .iter()
            .all(|byte| *byte == b'_' || byte.is_ascii_alphanumeric()))
    .then(|| std::str::from_utf8(raw).ok())
    .flatten())
}

fn function<'tree>(
    node: Node<'tree>,
    root: Node<'tree>,
) -> Option<(Node<'tree>, Node<'tree>, Node<'tree>, Node<'tree>)> {
    if node.kind() != "function_definition" || node.parent() != Some(root) {
        return None;
    }
    let declarator = node
        .child_by_field_name("declarator")
        .filter(|node| node.kind() == "function_declarator")?;
    let name = declarator
        .child_by_field_name("declarator")
        .filter(|node| node.kind() == "identifier")?;
    Some((
        declarator,
        name,
        declarator.child_by_field_name("parameters")?,
        node.child_by_field_name("body")?,
    ))
}

fn source_proofs_with_depth(
    source: &[u8],
    maximum: usize,
    depth_limit: usize,
    cancel: &Cancellation,
) -> Result<Vec<Written>, AdapterError> {
    let mut result = Vec::new();
    let scope = crate::runtime::cuda_scope::begin(cancel, maximum)?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_cuda::LANGUAGE.into())
        .map_err(|_| provider_failure("cuda-consumer-grammar"))?;
    let mut budget = maximum;
    let mut exhausted = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if cancel.check().is_err() {
            return ControlFlow::Break(());
        }
        if let Some(next) = budget.checked_sub(1) {
            budget = next;
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
    crate::runtime::cuda_scope::finish(scope, cancel, None)?;
    cancel.check()?;
    if exhausted {
        return Err(provider_failure("cuda-consumer-work-limit"));
    }
    let Some(tree) = tree else { return Ok(result) };
    let root = tree.root_node();
    if root.kind() != "translation_unit"
        || root.has_error()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
    {
        return Ok(result);
    }
    let mut work = Work {
        remaining: maximum,
        depth_limit,
    };
    let all = collect(root, &mut work, cancel)?;
    if all.len() > maximum / 64 {
        return Err(provider_failure("cuda-consumer-syntax-limit"));
    }
    for node in &all {
        work.step(cancel)?;
        if node.is_missing()
            || node.is_error()
            || (node.kind().starts_with("preproc_") && node.kind() != "preproc_include")
        {
            return Ok(result);
        }
    }
    let mut callables = HashMap::<&str, usize>::new();
    callables
        .try_reserve(all.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for node in &all {
        work.step(cancel)?;
        if node.kind() == "function_declarator"
            && let Some(name) = node.child_by_field_name("declarator")
            && let Some(word) = identifier(name, source, &mut work, cancel)?
        {
            *callables.entry(word).or_default() += 1;
        }
    }
    for owner in all.iter().copied() {
        work.step(cancel)?;
        let Some((_, owner_name, header, body)) = function(owner, root) else {
            continue;
        };
        let Some(owner_word) = identifier(owner_name, source, &mut work, cancel)? else {
            continue;
        };
        if callables.get(owner_word) != Some(&1) {
            continue;
        }
        let mut formals = HashMap::<&str, (Node<'_>, Node<'_>, bool)>::new();
        formals
            .try_reserve(header.named_child_count())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut valid = true;
        let mut cursor = header.walk();
        for parameter in header.named_children(&mut cursor) {
            work.step(cancel)?;
            if parameter.kind() == "comment" {
                continue;
            }
            if parameter.kind() != "parameter_declaration" {
                valid = false;
                break;
            }
            let names = collect(parameter, &mut work, cancel)?;
            let mut name = None;
            for child in names {
                work.step(cancel)?;
                if child.kind() == "identifier" && name.replace(child).is_some() {
                    valid = false;
                }
            }
            let Some(name) = name else {
                valid = false;
                break;
            };
            let Some(word) = identifier(name, source, &mut work, cancel)? else {
                valid = false;
                break;
            };
            let scalar = parameter.child_by_field_name("declarator") == Some(name)
                && parameter
                    .child_by_field_name("type")
                    .is_some_and(|kind| kind.kind() == "primitive_type");
            valid &= formals.insert(word, (parameter, name, scalar)).is_none();
        }
        if !valid {
            continue;
        }
        let nodes = collect(body, &mut work, cancel)?;
        let mut collisions = HashMap::<&str, ()>::new();
        collisions
            .try_reserve(nodes.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for node in &nodes {
            work.step(cancel)?;
            if matches!(
                node.kind(),
                "lambda_expression"
                    | "function_definition"
                    | "structured_binding_declarator"
                    | "for_range_loop"
                    | "catch_clause"
                    | "type_definition"
                    | "alias_declaration"
                    | "class_specifier"
                    | "struct_specifier"
                    | "union_specifier"
            ) {
                valid = false;
            }
            let Some(word) = identifier(*node, source, &mut work, cancel)? else {
                continue;
            };
            if node.parent().is_some_and(|parent| {
                matches!(
                    parent.kind(),
                    "init_declarator"
                        | "pointer_declarator"
                        | "reference_declarator"
                        | "declaration"
                ) && parent.child_by_field_name("declarator") == Some(*node)
            }) {
                collisions.insert(word, ());
            }
            if formals.contains_key(word)
                && node.parent().is_some_and(|parent| {
                    matches!(
                        parent.kind(),
                        "assignment_expression" | "update_expression" | "pointer_expression"
                    )
                })
            {
                valid = false;
            }
        }
        if !valid {
            continue;
        }
        for site in nodes
            .iter()
            .copied()
            .filter(|node| node.kind() == "identifier")
        {
            work.step(cancel)?;
            let Some(word) = identifier(site, source, &mut work, cancel)? else {
                continue;
            };
            if let Some((parameter, name, true)) = formals.get(word).copied()
                && !collisions.contains_key(word)
                && site
                    .parent()
                    .is_some_and(|parent| parent.kind() == "binary_expression")
            {
                result
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                result.push(Written {
                    facet: Facet::Read,
                    site: site.byte_range(),
                    owner: owner.byte_range(),
                    owner_name: owner_name.byte_range(),
                    header: header.byte_range(),
                    target: parameter.byte_range(),
                    target_name: name.byte_range(),
                    ancestors: written_ancestors(site, owner, &mut work, cancel)?,
                });
            }
            let Some(call) = site.parent().filter(|parent| {
                parent.kind() == "call_expression"
                    && parent.child_by_field_name("function") == Some(site)
            }) else {
                continue;
            };
            if formals.contains_key(word)
                || collisions.contains_key(word)
                || callables.get(word) != Some(&1)
            {
                continue;
            }
            let mut cursor = call.walk();
            if !call
                .named_children(&mut cursor)
                .any(|child| child.kind() == "kernel_call_syntax")
            {
                continue;
            }
            for target in all.iter().copied() {
                work.step(cancel)?;
                let Some((_, name, parameters, _)) = function(target, root) else {
                    continue;
                };
                let Some(arguments) = call.child_by_field_name("arguments") else {
                    continue;
                };
                let mut formal_count = 0_usize;
                let mut regular = true;
                let mut cursor = parameters.walk();
                for parameter in parameters.named_children(&mut cursor) {
                    work.step(cancel)?;
                    if parameter.kind() == "comment" {
                        continue;
                    }
                    regular &= parameter.kind() == "parameter_declaration";
                    formal_count = formal_count.saturating_add(1);
                }
                let mut actual_count = 0_usize;
                let mut cursor = arguments.walk();
                for argument in arguments.named_children(&mut cursor) {
                    work.step(cancel)?;
                    if argument.kind() != "comment" {
                        actual_count = actual_count.saturating_add(1);
                    }
                }
                if !regular || formal_count != actual_count {
                    continue;
                }
                let mut cursor = target.walk();
                if identifier(name, source, &mut work, cancel)? == Some(word)
                    && target
                        .children(&mut cursor)
                        .any(|child| child.kind() == "__global__")
                {
                    result
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    result.push(Written {
                        facet: Facet::Call,
                        site: site.byte_range(),
                        owner: owner.byte_range(),
                        owner_name: owner_name.byte_range(),
                        header: header.byte_range(),
                        target: target.byte_range(),
                        target_name: name.byte_range(),
                        ancestors: written_ancestors(site, owner, &mut work, cancel)?,
                    });
                }
            }
        }
    }
    Ok(result)
}

struct Binding {
    target: SymbolId,
    owner: SymbolId,
    facet: Facet,
    source: SourceRef,
    spelling: ContentHash,
    provenance: FactId,
}
pub(super) struct Targets {
    bindings: HashMap<u64, Binding>,
    source: SourceRef,
}

fn exact(span: SourceSpan, range: &Range<usize>) -> bool {
    usize::try_from(span.start_byte()).ok() == Some(range.start)
        && usize::try_from(span.end_byte()).ok() == Some(range.end)
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.span().file() == right.span().file()
        && left.content_hash() == right.content_hash()
}

// Top-level functions remain owned by the physical file after materialization.
fn file_owner(entity: &MaterializedEntity, source: &SourceRef) -> bool {
    let expected = ContainerRef::File(source.span().file());
    entity.direct_parent == expected
        && entity.record.container == Some(expected)
        && entity.identity_claim.container == Some(expected)
}

impl Targets {
    /// Join independent current CST endpoints to unique exact raw/entity witnesses.
    #[expect(
        clippy::too_many_arguments,
        reason = "the proof boundary receives independent source, quota and coordinate authorities"
    )]
    pub(super) fn new(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        bytes: &[u8],
        physical_bytes: &[u8],
        maximum: usize,
        depth_limit: usize,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            bindings: HashMap::new(),
            source: source.clone(),
        };
        if content_hash(physical_bytes) != source.content_hash()
            || u64::try_from(physical_bytes.len()).ok() != Some(source.span().end_byte())
        {
            return Ok(result);
        }
        let mut by_id = HashMap::new();
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut root = None;
        let mut clean = None;
        let mut invalid = false;
        let mut counts = HashMap::new();
        counts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            invalid |= by_id.insert(fact.local_id(), fact).is_some();
            invalid |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            *counts
                .entry((fact.kind(), fact.syntax_kind().as_str(), fact.span()))
                .or_insert(0_usize) += 1;
            if fact.kind() == SyntaxFactKind::Root {
                invalid |= root.replace(fact).is_some()
                    || fact.parent().is_some()
                    || fact.depth() != 0
                    || !matches!(
                        fact.syntax_kind().as_str(),
                        "cpp.file.root" | "cuda.file.root"
                    )
                    || physical(fact.span())? != source.span();
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "cuda.clean_file.signature"
            {
                invalid |= clean.replace(fact).is_some() || physical(fact.span())? != source.span();
            }
        }
        let (Some(root), Some(clean)) = (root, clean) else {
            return Ok(result);
        };
        if invalid
            || source.span().start_byte() != 0
            || clean.parent() != Some(root.local_id())
            || clean.depth() != 1
        {
            return Ok(result);
        }
        let source_language = if root.syntax_kind().as_str() == "cpp.file.root" {
            "cpp"
        } else {
            "cuda"
        };
        let proofs = source_proofs_with_depth(bytes, maximum, depth_limit, cancel)?;
        let raw_work = Cell::new(maximum);
        let raw = |kind, range: &Range<usize>, suffix: &str| -> Option<&SyntaxFact> {
            let mut selected = None;
            for fact in facts {
                cancel.check().ok()?;
                raw_work.set(raw_work.get().checked_sub(1)?);
                if fact.kind() == kind
                    && exact(fact.span(), range)
                    && if suffix.starts_with('.') {
                        fact.syntax_kind().as_str().strip_prefix(source_language) == Some(suffix)
                    } else {
                        fact.syntax_kind().as_str() == suffix
                    }
                    && (counts.get(&(fact.kind(), fact.syntax_kind().as_str(), fact.span()))
                        != Some(&1)
                        || selected.replace(fact).is_some())
                {
                    return None;
                }
            }
            selected
        };
        for proof in proofs {
            cancel.check()?;
            let Some(owner_fact) = raw(
                SyntaxFactKind::Declaration,
                &proof.owner,
                ".function.declaration",
            ) else {
                continue;
            };
            let Some(owner_name) = raw(
                SyntaxFactKind::Occurrence,
                &proof.owner_name,
                ".identifier.definition",
            ) else {
                continue;
            };
            let Some(owner) = entities.get(&owner_fact.local_id()) else {
                continue;
            };
            let Some(reference) = raw(
                SyntaxFactKind::Occurrence,
                &proof.site,
                ".identifier.reference",
            ) else {
                continue;
            };
            let Some(context) = raw(
                SyntaxFactKind::Signature,
                &proof.owner,
                "cuda.owned_function.signature",
            ) else {
                continue;
            };
            let Some(header) = raw(
                SyntaxFactKind::Signature,
                &proof.header,
                ".parameters.signature",
            ) else {
                continue;
            };
            let target_suffix = if proof.facet == Facet::Read {
                ".parameter.declaration"
            } else {
                ".function.declaration"
            };
            let target_fact = if proof.facet == Facet::Read {
                raw(
                    SyntaxFactKind::Declaration,
                    &proof.target,
                    ".ordinary_parameter.declaration",
                )
                .or_else(|| raw(SyntaxFactKind::Declaration, &proof.target, target_suffix))
            } else {
                raw(SyntaxFactKind::Declaration, &proof.target, target_suffix)
            };
            let Some(target_fact) = target_fact else {
                continue;
            };
            let Some(target) = entities.get(&target_fact.local_id()) else {
                continue;
            };
            let Some(target_name) = raw(
                SyntaxFactKind::Occurrence,
                &proof.target_name,
                ".identifier.definition",
            ) else {
                continue;
            };
            let marker_range = if proof.facet == Facet::Call {
                proof.site.clone()
            } else {
                proof.target_name.start..proof.site.end
            };
            let marker_label = if proof.facet == Facet::Read {
                READ_TARGET
            } else {
                CALL_TARGET
            };
            let Some(marker) = raw(SyntaxFactKind::Signature, &marker_range, marker_label) else {
                continue;
            };
            let Some(owner_source) = &owner.record.evidence.source else {
                continue;
            };
            let Some(target_source) = &target.record.evidence.source else {
                continue;
            };
            let Some(spelling) = bytes.get(proof.site.clone()) else {
                continue;
            };
            let Some(target_text) = bytes.get(proof.target_name.clone()) else {
                continue;
            };
            let Some(owner_text) = bytes.get(proof.owner_name.clone()) else {
                continue;
            };
            let mut expected_parent = reference.parent();
            let mut ancestry_matches = true;
            for (position, (range, scope)) in proof.ancestors.iter().enumerate() {
                cancel.check()?;
                let Some(ancestor) = raw(
                    if *scope {
                        SyntaxFactKind::Scope
                    } else {
                        SyntaxFactKind::Declaration
                    },
                    range,
                    if *scope {
                        ".block.scope"
                    } else {
                        ".declaration.declaration"
                    },
                ) else {
                    ancestry_matches = false;
                    break;
                };
                // Fact depth counts captured structural containers, not raw CST edges.
                // Cpp/CUDA captures use zero native tie depth; the independently
                // captured native ancestor list determines the exact fact depth.
                let expected_depth = proof.ancestors.len() - position + 1;
                if expected_parent != Some(ancestor.local_id())
                    || ancestor.depth() != expected_depth
                {
                    ancestry_matches = false;
                    break;
                }
                expected_parent = ancestor.parent();
            }
            ancestry_matches &= expected_parent == Some(owner_fact.local_id());
            if !ancestry_matches {
                continue;
            }
            let reference_depth = proof
                .ancestors
                .len()
                .checked_add(2)
                .ok_or_else(|| provider_failure("cuda-consumer-syntax-limit"))?;
            let marker_matches = if proof.facet == Facet::Read {
                marker.parent() == Some(owner_fact.local_id()) && marker.depth() == 2
            } else {
                marker.parent() == reference.parent() && marker.depth() == reference_depth
            };
            if owner_fact.parent() != Some(root.local_id())
                || owner_fact.depth() != 1
                || !file_owner(owner, source)
                || owner_name.depth() != 2
                || reference.depth() != reference_depth
                || context.parent() != Some(owner_fact.local_id())
                || context.depth() != 2
                || header.parent() != Some(owner_fact.local_id())
                || header.depth() != 2
                || !marker_matches
                || owner.record.kind != EntityKind::Function
                || !matches!(owner.record.language.as_str(), "cuda" | "cpp")
                || owner.record.language != source_language
                || target.record.language != owner.record.language
                || owner.record.canonical_name.as_bytes() != owner_text
                || target.record.canonical_name.as_bytes() != target_text
                || target_text != spelling
                || !same_source(owner_source, source)
                || !same_source(target_source, source)
                || owner.record.repository != source.repository()
                || owner.record.generation != source.generation()
                || target.record.repository != source.repository()
                || target.record.generation != source.generation()
                || owner.record.provenance != target.record.provenance
                || owner.definition_local_id != Some(owner_name.local_id())
                || target.definition_local_id != Some(target_name.local_id())
                || owner_name.parent() != Some(owner_fact.local_id())
                || target_name.parent() != Some(target_fact.local_id())
                || physical(owner_fact.span())? != owner_source.span()
                || physical(target_fact.span())? != target_source.span()
            {
                continue;
            }
            if proof.facet == Facet::Read
                && (target.record.kind != EntityKind::Parameter
                    || target.direct_parent != ContainerRef::Entity(owner.record.id)
                    || target.record.container != Some(target.direct_parent)
                    || target_fact.parent() != Some(owner_fact.local_id())
                    || target.identity_claim.container != Some(target.direct_parent)
                    || target_fact.depth() != 2
                    || target_name.depth() != 3)
            {
                continue;
            }
            if proof.facet == Facet::Call {
                // A defined callee's required native function context is part
                // of its certificate, alongside the declaration and Definition.
                let Some(target_context) = raw(
                    SyntaxFactKind::Signature,
                    &proof.target,
                    "cuda.owned_function.signature",
                ) else {
                    continue;
                };
                if target.record.kind != EntityKind::Function
                    || target_fact.parent() != Some(root.local_id())
                    || target_fact.depth() != 1
                    || target_name.depth() != 2
                    || !file_owner(target, source)
                    || target_context.span() != target_fact.span()
                    || target_context.parent() != Some(target_fact.local_id())
                    || target_context.depth() != 2
                {
                    continue;
                }
            }
            result
                .bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if result
                .bindings
                .insert(
                    reference.local_id(),
                    Binding {
                        target: target.record.id,
                        owner: owner.record.id,
                        facet: proof.facet,
                        source: super::source_for_span(source, physical(reference.span())?),
                        spelling: content_hash(spelling),
                        provenance: owner.record.provenance,
                    },
                )
                .is_some()
            {
                return Ok(Self {
                    bindings: HashMap::new(),
                    source: source.clone(),
                });
            }
        }
        cancel.check()?;
        if raw_work.get() == 0 {
            return Err(provider_failure("cuda-consumer-work-limit"));
        }
        Ok(result)
    }

    /// Return one additional facet without mutating the original Reference.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<(Facet, SymbolId)>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        if !reference(fact)
            || occurrence.role != OccurrenceRole::Reference
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.source != binding.source
            || occurrence.enclosing != Some(binding.owner)
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.provenance != binding.provenance
            || occurrence.syntactic_text_hash != binding.spelling
        {
            return Ok(None);
        }
        Ok(Some((binding.facet, binding.target)))
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn source_proofs(
        source: &[u8],
        maximum: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<Written>, AdapterError> {
        source_proofs_with_depth(source, maximum, 128, cancel)
    }

    #[test]
    fn independent_original_cuda_and_cpp_header_endpoints_are_exact() {
        let source =
            include_bytes!("../../tests/fixtures/cuda_original/dev/cuda/crossentropy_forward.cu");
        let found = source_proofs(source, 1_048_576, &Cancellation::new()).unwrap();
        assert_eq!(
            found
                .iter()
                .filter(|proof| proof.facet == Facet::Call
                    && proof.site == (2107..2135)
                    && proof.target_name == (1259..1287)
                    && proof.owner_name == (1815..1836))
                .count(),
            1
        );
        let header = include_bytes!("../../tests/fixtures/cuda_original/llmc/fused_classifier.cuh");
        let found = source_proofs(header, 1_048_576, &Cancellation::new()).unwrap();
        assert_eq!(
            found
                .iter()
                .filter(|proof| proof.facet == Facet::Read
                    && proof.site == (682..685)
                    && proof.target_name == (528..531)
                    && proof.owner_name == (493..519))
                .count(),
            1
        );
        assert!(
            !found
                .iter()
                .any(|proof| proof.facet == Facet::Call && proof.site == (3479..3505))
        );
    }

    #[test]
    fn independent_consumer_declines_ambiguous_recovered_and_non_native_launches() {
        for source in [
            "int owned(int value, int value) { return value * 2; }",
            "int owned(int value) { int value = 1; return value * 2; }",
            "int owned(int value) { value++; return value * 2; }",
            "int owned(int value) { auto nested = [&] { return value * 2; }; }",
            "template<class T> int owned(int value) { return value * 2; }",
            "__global__ void kernel() {} void kernel(int value) {} void launch() { kernel<<<1,1>>>(); }",
            "__global__ void kernel() {} void launch() { kernel(); }",
            "int owned(int value) { return value * ; }",
        ] {
            assert!(
                source_proofs(source.as_bytes(), 65_536, &Cancellation::new())
                    .unwrap()
                    .is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn independent_consumer_cancellation_and_finite_census_are_observable_refusals() {
        let source = b"int owned(int value) { return value * 2; }";
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(source_proofs(source, 65_536, &cancel).is_err());
        assert!(source_proofs(source, 0, &Cancellation::new()).is_err());
        assert_eq!(
            source_proofs(source, 65_536, &Cancellation::new())
                .unwrap()
                .len(),
            1
        );
    }
}
