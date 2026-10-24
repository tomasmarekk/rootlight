//! Reconstruct ordinary PHP scopes independently before admitting native parameters.
//! Materialized bindings require exact raw topology, physical bytes and native provenance.

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    ops::ControlFlow,
};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FileRecord, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};
use tree_sitter::{Node, ParseOptions};

use super::{MaterializedEntity, lexical_reference_relation, provider_failure, source_for_span};

type Span = (usize, usize);
pub(super) const CALL: &str = "php.ordinary_function_call.call";
pub(super) const GAP: &str = "php-written-function-runtime-and-external-effects-unavailable";

pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "php.ordinary_clean_file.signature"
                | "php.ordinary_function_context.signature"
                | "php.ordinary_formal_target.signature"
                | "php.ordinary_call_target.signature"
                | "php.ordinary_call_context.signature"
        )
}

#[derive(Default)]
pub(super) struct Requirements {
    clean: HashSet<rootlight_ids::FileId>,
    names: HashSet<SourceSpan>,
    calls: HashSet<SourceSpan>,
}
impl Requirements {
    pub(super) fn new(facts: &[SyntaxFact], cancel: &Cancellation) -> Result<Self, AdapterError> {
        let mut value = Self::default();
        for fact in facts {
            cancel.check()?;
            if fact.kind() != SyntaxFactKind::Signature {
                continue;
            }
            match fact.syntax_kind().as_str() {
                "php.ordinary_clean_file.signature" => {
                    value
                        .clean
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    value.clean.insert(fact.span().file());
                }
                "php.ordinary_call_target.signature" => {
                    value
                        .names
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    value.names.insert(fact.span());
                }
                "php.ordinary_call_context.signature" => {
                    value
                        .calls
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    value.calls.insert(fact.span());
                }
                _ => {}
            }
        }
        Ok(value)
    }
    pub(super) fn call_name(&self, fact: &SyntaxFact) -> bool {
        fact.kind() == SyntaxFactKind::Occurrence
            && fact.syntax_kind().as_str() == "php.name.reference"
            && self.names.contains(&fact.span())
    }
    pub(super) fn required(&self, fact: &SyntaxFact) -> bool {
        (fact.kind() == SyntaxFactKind::Occurrence
            && fact.syntax_kind().as_str() == "php.ordinary_formal.reference")
            || self.call_name(fact)
            || (fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "php.call.call"
                && self.calls.contains(&fact.span()))
            || (fact.kind() == SyntaxFactKind::Scope
                && self.clean.contains(&fact.span().file())
                && matches!(
                    fact.syntax_kind().as_str(),
                    "php.block.scope" | "php.declaration_list.scope"
                ))
    }
}

#[derive(Clone, Copy)]
struct Folded<'a>(&'a str);
impl PartialEq for Folded<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(other.0)
    }
}
impl Eq for Folded<'_> {}
impl Hash for Folded<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for byte in self.0.bytes() {
            state.write_u8(byte.to_ascii_lowercase());
        }
        state.write_u8(0);
    }
}

struct Work {
    remaining: usize,
}
impl Work {
    fn step(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        cancel.check()?;
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| provider_failure("php-ordinary-consumer-work-limit"))?;
        Ok(())
    }
}

fn extent(node: Node<'_>) -> Span {
    (node.start_byte(), node.end_byte())
}
fn span(fact: &SyntaxFact) -> Option<Span> {
    Some((
        usize::try_from(fact.span().start_byte()).ok()?,
        usize::try_from(fact.span().end_byte()).ok()?,
    ))
}
fn name<'source>(
    source: &'source [u8],
    node: Node<'_>,
    variable: bool,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<&'source str>, AdapterError> {
    work.step(cancel)?;
    let Some(raw) = source.get(node.byte_range()) else {
        return Ok(None);
    };
    let bare = if variable {
        raw.strip_prefix(b"$")
    } else {
        Some(raw)
    };
    let Some(bare) = bare else { return Ok(None) };
    let Some((&first, _)) = bare.split_first() else {
        return Ok(None);
    };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return Ok(None);
    }
    for bytes in bare.chunks(64) {
        work.step(cancel)?;
        if !bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            return Ok(None);
        }
    }
    Ok(std::str::from_utf8(raw).ok())
}

struct Function<'tree, 'source> {
    node: Node<'tree>,
    name: &'source str,
    name_node: Node<'tree>,
    parameters: Node<'tree>,
    body: Node<'tree>,
    formals: HashMap<&'source str, Span>,
    admitted: bool,
}

fn function<'tree, 'source>(
    source: &'source [u8],
    node: Node<'tree>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Function<'tree, 'source>>, AdapterError> {
    let Some(name_node) = node.child_by_field_name("name") else {
        return Ok(None);
    };
    let Some(written_name) = name(source, name_node, false, work, cancel)? else {
        return Ok(None);
    };
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return Ok(None);
    };
    let Some(body) = node.child_by_field_name("body") else {
        return Ok(None);
    };
    let mut value = Function {
        node,
        name: written_name,
        name_node,
        parameters,
        body,
        formals: HashMap::new(),
        admitted: parameters.kind() == "formal_parameters" && body.kind() == "compound_statement",
    };
    let mut cursor = node.walk();
    for field in node.named_children(&mut cursor) {
        work.step(cancel)?;
        value.admitted &= matches!(
            field.kind(),
            "name" | "formal_parameters" | "compound_statement"
        );
    }
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        work.step(cancel)?;
        if parameter.kind() != "simple_parameter"
            || parameter.child_by_field_name("type").is_some()
            || parameter.child_by_field_name("attributes").is_some()
            || parameter
                .child_by_field_name("reference_modifier")
                .is_some()
            || parameter
                .child_by_field_name("default_value")
                .is_some_and(|n| !matches!(n.kind(), "integer" | "float"))
        {
            value.admitted = false;
            continue;
        }
        let Some(variable) = parameter
            .child_by_field_name("name")
            .filter(|n| n.kind() == "variable_name")
        else {
            value.admitted = false;
            continue;
        };
        let Some(formal_name) = name(source, variable, true, work, cancel)? else {
            value.admitted = false;
            continue;
        };
        if formal_name == "$this" || formal_name == "$GLOBALS" || formal_name.starts_with("$_") {
            value.admitted = false;
        }
        value
            .formals
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        if value
            .formals
            .insert(formal_name, extent(variable))
            .is_some()
        {
            value.admitted = false;
        }
    }
    let mut cursor = body.walk();
    loop {
        work.step(cancel)?;
        let current = cursor.node();
        match current.kind() {
            "anonymous_function"
            | "arrow_function"
            | "anonymous_class"
            | "function_definition"
            | "foreach_statement"
            | "catch_clause"
            | "global_declaration"
            | "function_static_declaration"
            | "unset_statement"
            | "reference_assignment_expression"
            | "dynamic_variable_name"
            | "reference_modifier"
            | "variadic_unpacking" => value.admitted = false,
            "assignment_expression" | "augmented_assignment_expression" | "update_expression" => {
                let field = if current.kind() == "update_expression" {
                    "argument"
                } else {
                    "left"
                };
                if let Some(left) = current
                    .child_by_field_name(field)
                    .filter(|n| n.kind() == "variable_name")
                {
                    match name(source, left, true, work, cancel)? {
                        Some(written) => value.admitted &= !value.formals.contains_key(written),
                        None => value.admitted = false,
                    }
                } else {
                    value.admitted = false;
                }
            }
            "function_call_expression" => {
                if let Some(callee) = current
                    .child_by_field_name("function")
                    .filter(|n| n.kind() == "name")
                {
                    match name(source, callee, false, work, cancel)? {
                        Some(written) => {
                            value.admitted &= !["eval", "extract", "parse_str"]
                                .iter()
                                .any(|effect| written.eq_ignore_ascii_case(effect))
                        }
                        None => value.admitted = false,
                    }
                } else {
                    value.admitted = false;
                }
            }
            _ => {}
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(Some(value));
            }
        }
    }
}

struct Frame<'facts> {
    by_key: HashMap<(&'facts str, Span), Option<&'facts SyntaxFact>>,
}
impl<'facts> Frame<'facts> {
    fn new(
        facts: &'facts [SyntaxFact],
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut by_key: HashMap<(&str, Span), Option<&SyntaxFact>> = HashMap::new();
        by_key
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut ids = HashSet::new();
        let mut roots = 0usize;
        ids.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            work.step(cancel)?;
            if !ids.insert(fact.local_id()) || fact.kind() == SyntaxFactKind::ErrorRecovery {
                return Err(provider_failure("php-ordinary-raw-frame"));
            }
            roots += usize::from(fact.kind() == SyntaxFactKind::Root);
            let Some(range) = span(fact) else {
                return Err(provider_failure("php-ordinary-raw-frame"));
            };
            by_key
                .entry((fact.syntax_kind().as_str(), range))
                .and_modify(|entry| *entry = None)
                .or_insert(Some(fact));
        }
        if roots != 1 {
            return Err(provider_failure("php-ordinary-root-witness"));
        }
        Ok(Self { by_key })
    }
    fn get(
        &self,
        label: &str,
        range: Span,
        kind: SyntaxFactKind,
        parent: Option<u64>,
        depth: usize,
    ) -> Option<&'facts SyntaxFact> {
        self.by_key
            .get(&(label, range))
            .copied()
            .flatten()
            .filter(|fact| fact.kind() == kind && fact.parent() == parent && fact.depth() == depth)
    }
}

#[derive(Clone)]
struct RawEntity {
    local: u64,
    definition: u64,
    name: String,
    physical: SourceSpan,
    header: Option<SourceSpan>,
    signature: Vec<u8>,
    owner: Option<u64>,
}
struct RawBinding {
    owner: u64,
    target: u64,
    physical: SourceSpan,
    spelling: rootlight_ids::ContentHash,
}

#[derive(Default)]
pub(super) struct Plan {
    functions: HashMap<u64, RawEntity>,
    parameters: HashMap<u64, RawEntity>,
    references: HashMap<u64, RawBinding>,
    calls: HashMap<u64, RawBinding>,
}

fn copy_name(value: &str) -> Result<String, AdapterError> {
    let mut result = String::new();
    result
        .try_reserve_exact(value.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    result.push_str(value);
    Ok(result)
}

fn block_parent<'facts>(
    frame: &Frame<'facts>,
    node: Node<'_>,
    function: &Function<'_, '_>,
    owner: u64,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<&'facts SyntaxFact>, AdapterError> {
    let mut current = node.parent();
    let mut scopes = Vec::new();
    while let Some(parent) = current {
        work.step(cancel)?;
        if parent == function.node {
            break;
        }
        if parent.kind() == "compound_statement" {
            scopes
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            scopes.push(extent(parent));
        }
        current = parent.parent();
    }
    if current != Some(function.node) || scopes.is_empty() {
        return Ok(None);
    }
    let mut expected_parent = owner;
    let mut depth = 2usize;
    let mut result = None;
    for scope in scopes.into_iter().rev() {
        work.step(cancel)?;
        let Some(raw) = frame.get(
            "php.block.scope",
            scope,
            SyntaxFactKind::Scope,
            Some(expected_parent),
            depth,
        ) else {
            return Ok(None);
        };
        expected_parent = raw.local_id();
        depth = depth
            .checked_add(1)
            .ok_or_else(|| provider_failure("php-ordinary-depth"))?;
        result = Some(raw);
    }
    Ok(result)
}

fn function_blocks(
    frame: &Frame<'_>,
    function: &Function<'_, '_>,
    owner: u64,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = function.body.walk();
    loop {
        work.step(cancel)?;
        let node = cursor.node();
        if node.kind() == "compound_statement" {
            let (parent, depth) = if node == function.body {
                (owner, 2)
            } else {
                let Some(parent) = block_parent(frame, node, function, owner, work, cancel)? else {
                    return Ok(false);
                };
                let depth = parent
                    .depth()
                    .checked_add(1)
                    .ok_or_else(|| provider_failure("php-ordinary-depth"))?;
                (parent.local_id(), depth)
            };
            if frame
                .get(
                    "php.block.scope",
                    extent(node),
                    SyntaxFactKind::Scope,
                    Some(parent),
                    depth,
                )
                .is_none()
            {
                return Ok(false);
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(true);
            }
        }
    }
}

impl Plan {
    /// Reparse the exact current view and validate mandatory raw witnesses.
    /// New parameters are withheld before entity drafting when a witness is absent.
    pub(super) fn new(
        facts: &[SyntaxFact],
        source: &SourceRef,
        current: &[u8],
        physical_bytes: &[u8],
        maximum: usize,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut claimed = false;
        for fact in facts {
            cancel.check()?;
            claimed |= fact.syntax_kind().as_str().starts_with("php.ordinary_");
        }
        if !claimed {
            return Ok(Self::default());
        }
        let mut work = Work { remaining: maximum };
        if source.span().start_byte() != 0
            || u64::try_from(physical_bytes.len()).ok() != Some(source.span().end_byte())
        {
            return Err(provider_failure("php-ordinary-source-frame"));
        }
        let mut hasher = blake3::Hasher::new();
        for chunk in physical_bytes.chunks(64) {
            work.step(cancel)?;
            hasher.update(chunk);
        }
        if rootlight_ids::ContentHash::from_bytes(*hasher.finalize().as_bytes())
            != source.content_hash()
        {
            return Err(provider_failure("php-ordinary-source-frame"));
        }
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .map_err(|_| provider_failure("php-ordinary-consumer-grammar"))?;
        let mut parse_work = work.remaining;
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
        let mut input =
            |offset: usize, _: tree_sitter::Point| current.get(offset..).unwrap_or_default();
        let tree = parser.parse_with_options(
            &mut input,
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        cancel.check()?;
        if exhausted {
            return Err(provider_failure("php-ordinary-consumer-work-limit"));
        }
        work.remaining = parse_work;
        let tree = tree.ok_or_else(|| provider_failure("php-ordinary-current-syntax"))?;
        let root = tree.root_node();
        if root.kind() != "program"
            || root.has_error()
            || root.is_missing()
            || root.end_byte() != current.len()
            || !matches!(
                current.get(..root.start_byte()),
                Some(b"" | b"\xef\xbb\xbf")
            )
        {
            return Err(provider_failure("php-ordinary-current-syntax"));
        }
        let mut functions = Vec::new();
        let mut by_name = HashMap::new();
        let mut by_span = HashMap::new();
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            work.step(cancel)?;
            match child.kind() {
                "comment" | "php_tag" => {}
                "text"
                    if current.get(child.byte_range()).is_some_and(|bytes| {
                        bytes
                            .strip_prefix(b"\xef\xbb\xbf")
                            .unwrap_or(bytes)
                            .iter()
                            .all(u8::is_ascii_whitespace)
                    }) => {}
                "function_definition" => {
                    let item = function(current, child, &mut work, cancel)?
                        .ok_or_else(|| provider_failure("php-ordinary-current-syntax"))?;
                    by_name
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    by_span
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    if by_name.insert(Folded(item.name), functions.len()).is_some() {
                        return Err(provider_failure("php-ordinary-function-collision"));
                    }
                    by_span.insert(extent(child), functions.len());
                    functions
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    functions.push(item);
                }
                _ => return Err(provider_failure("php-ordinary-current-syntax")),
            }
        }
        let mut cursor = root.walk();
        loop {
            work.step(cancel)?;
            let node = cursor.node();
            if matches!(
                node.kind(),
                "include_expression"
                    | "include_once_expression"
                    | "require_expression"
                    | "require_once_expression"
                    | "eval_expression"
                    | "namespace_definition"
                    | "namespace_use_declaration"
            ) || (node.kind() == "function_definition" && node.parent() != Some(root))
            {
                return Err(provider_failure("php-ordinary-current-syntax"));
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
        let frame = Frame::new(facts, &mut work, cancel)?;
        let native_root_range = extent(root);
        // Native roots omit a leading BOM; the file-root fact includes it.
        // The clean-file signature still certifies the exact native extent.
        let raw_root_range = if root.start_byte() == 3 && current.starts_with(b"\xef\xbb\xbf") {
            (0, root.end_byte())
        } else {
            native_root_range
        };
        let raw_root = frame
            .get(
                "php.program.root",
                raw_root_range,
                SyntaxFactKind::Root,
                None,
                0,
            )
            .ok_or_else(|| provider_failure("php-ordinary-root-witness"))?;
        let root_physical = physical(raw_root.span())?;
        if root_physical != source.span()
            || frame
                .get(
                    "php.ordinary_clean_file.signature",
                    native_root_range,
                    SyntaxFactKind::Signature,
                    Some(raw_root.local_id()),
                    1,
                )
                .is_none()
        {
            return Err(provider_failure("php-ordinary-clean-witness"));
        }
        let mut result = Self::default();
        let mut function_locals = HashMap::new();
        for item in &functions {
            work.step(cancel)?;
            if !item.admitted {
                continue;
            }
            let Some(declaration) = frame.get(
                "php.function.declaration",
                extent(item.node),
                SyntaxFactKind::Declaration,
                Some(raw_root.local_id()),
                1,
            ) else {
                continue;
            };
            let Some(definition) = frame.get(
                "php.name.definition",
                extent(item.name_node),
                SyntaxFactKind::Occurrence,
                Some(declaration.local_id()),
                2,
            ) else {
                continue;
            };
            if frame
                .get(
                    "php.ordinary_function_context.signature",
                    extent(item.node),
                    SyntaxFactKind::Signature,
                    Some(declaration.local_id()),
                    2,
                )
                .is_none()
                || frame
                    .get(
                        "php.parameters.signature",
                        extent(item.parameters),
                        SyntaxFactKind::Signature,
                        Some(declaration.local_id()),
                        2,
                    )
                    .is_none()
            {
                continue;
            }
            if !function_blocks(&frame, item, declaration.local_id(), &mut work, cancel)? {
                continue;
            }
            let Some(header) = current
                .get(item.parameters.byte_range())
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
            else {
                continue;
            };
            for _ in header.as_bytes().chunks(64) {
                work.step(cancel)?;
            }
            let Some(signature) = rootlight_ir::canonical_symbol_signature(header, header.len())
            else {
                continue;
            };
            let raw = RawEntity {
                local: declaration.local_id(),
                definition: definition.local_id(),
                name: copy_name(item.name)?,
                physical: physical(declaration.span())?,
                header: frame
                    .by_key
                    .get(&("php.parameters.signature", extent(item.parameters)))
                    .copied()
                    .flatten()
                    .map(SyntaxFact::span),
                signature: signature.into_bytes(),
                owner: None,
            };
            function_locals
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            function_locals.insert(extent(item.node), raw.local);
            result
                .functions
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.functions.insert(raw.local, raw);
            for (written, range) in &item.formals {
                work.step(cancel)?;
                let Some(parameter) = frame.get(
                    "php.ordinary_parameter.declaration",
                    *range,
                    SyntaxFactKind::Declaration,
                    Some(declaration.local_id()),
                    2,
                ) else {
                    continue;
                };
                let Some(definition) = frame.get(
                    "php.variable_name.definition",
                    *range,
                    SyntaxFactKind::Occurrence,
                    Some(parameter.local_id()),
                    3,
                ) else {
                    continue;
                };
                result
                    .parameters
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                result.parameters.insert(
                    parameter.local_id(),
                    RawEntity {
                        local: parameter.local_id(),
                        definition: definition.local_id(),
                        name: copy_name(written)?,
                        physical: physical(parameter.span())?,
                        header: None,
                        signature: Vec::new(),
                        owner: Some(declaration.local_id()),
                    },
                );
            }
        }
        let mut parameter_locals = HashMap::new();
        parameter_locals
            .try_reserve(result.parameters.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for raw in result.parameters.values() {
            work.step(cancel)?;
            parameter_locals.insert((raw.owner, raw.name.as_str()), raw.local);
        }
        for fact in facts {
            work.step(cancel)?;
            let label = fact.syntax_kind().as_str();
            if label.starts_with("php.ordinary_")
                && !matches!(
                    label,
                    "php.ordinary_clean_file.signature"
                        | "php.ordinary_function_context.signature"
                        | "php.ordinary_parameter.declaration"
                        | "php.ordinary_formal_target.signature"
                        | "php.ordinary_call_target.signature"
                        | "php.ordinary_call_context.signature"
                        | "php.ordinary_formal.reference"
                        | "php.ordinary_unproved_variable.reference"
                )
            {
                return Err(provider_failure("php-ordinary-native-label"));
            }
            if fact.kind() != SyntaxFactKind::Occurrence {
                continue;
            }
            if label == "php.variable_name.reference" {
                return Err(provider_failure("php-ordinary-legacy-variable-alias"));
            }
            if !matches!(
                label,
                "php.ordinary_formal.reference" | "php.name.reference"
            ) {
                continue;
            }
            let Some(range) = span(fact) else { continue };
            let Some(node) = root
                .descendant_for_byte_range(range.0, range.1)
                .filter(|n| extent(*n) == range)
            else {
                continue;
            };
            let mut ancestor = node.parent();
            let mut owner = None;
            while let Some(parent) = ancestor {
                work.step(cancel)?;
                if parent.kind() == "function_definition" {
                    owner = by_span.get(&extent(parent)).copied();
                    break;
                }
                ancestor = parent.parent();
            }
            let Some(item) = owner
                .and_then(|index| functions.get(index))
                .filter(|f| f.admitted)
            else {
                continue;
            };
            if node.start_byte() < item.body.start_byte() || node.end_byte() > item.body.end_byte()
            {
                continue;
            }
            let Some(owner) = function_locals.get(&extent(item.node)).copied() else {
                continue;
            };
            let Some(block) = block_parent(&frame, node, item, owner, &mut work, cancel)? else {
                continue;
            };
            let depth = block
                .depth()
                .checked_add(1)
                .ok_or_else(|| provider_failure("php-ordinary-depth"))?;
            if frame
                .get(
                    label,
                    range,
                    SyntaxFactKind::Occurrence,
                    Some(block.local_id()),
                    depth,
                )
                .is_none_or(|raw| raw.local_id() != fact.local_id())
            {
                continue;
            }
            if label == "php.ordinary_formal.reference" && node.kind() == "variable_name" {
                let Some(written) = name(current, node, true, &mut work, cancel)? else {
                    continue;
                };
                let Some(target) = parameter_locals.get(&(Some(owner), written)).copied() else {
                    continue;
                };
                if frame
                    .get(
                        "php.ordinary_formal_target.signature",
                        range,
                        SyntaxFactKind::Signature,
                        Some(block.local_id()),
                        depth,
                    )
                    .is_none()
                {
                    continue;
                }
                result
                    .references
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                result.references.insert(
                    fact.local_id(),
                    RawBinding {
                        owner,
                        target,
                        physical: physical(fact.span())?,
                        spelling: content_hash(written.as_bytes()),
                    },
                );
            }
            if label == "php.name.reference" && node.kind() == "name" {
                let Some(call) = node.parent().filter(|n| {
                    n.kind() == "function_call_expression"
                        && n.child_by_field_name("function") == Some(node)
                }) else {
                    continue;
                };
                let Some(written) = name(current, node, false, &mut work, cancel)? else {
                    continue;
                };
                let Some(target_item) = by_name
                    .get(&Folded(written))
                    .and_then(|index| functions.get(*index))
                    .filter(|item| item.admitted)
                else {
                    continue;
                };
                let Some(target) = function_locals.get(&extent(target_item.node)).copied() else {
                    continue;
                };
                let Some(arguments) = call.child_by_field_name("arguments") else {
                    continue;
                };
                let mut ordinary = true;
                let mut cursor = arguments.walk();
                for argument in arguments.named_children(&mut cursor) {
                    work.step(cancel)?;
                    ordinary &= argument.kind() == "comment"
                        || (argument.kind() == "argument"
                            && argument.child_by_field_name("name").is_none()
                            && argument.child_by_field_name("reference_modifier").is_none());
                }
                if !ordinary
                    || frame
                        .get(
                            "php.ordinary_call_target.signature",
                            range,
                            SyntaxFactKind::Signature,
                            Some(block.local_id()),
                            depth,
                        )
                        .is_none()
                    || frame
                        .get(
                            "php.ordinary_call_context.signature",
                            extent(call),
                            SyntaxFactKind::Signature,
                            Some(block.local_id()),
                            depth,
                        )
                        .is_none()
                    || frame
                        .get(
                            "php.call.call",
                            extent(call),
                            SyntaxFactKind::Occurrence,
                            Some(block.local_id()),
                            depth,
                        )
                        .is_none()
                {
                    continue;
                }
                result
                    .calls
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                result.calls.insert(
                    fact.local_id(),
                    RawBinding {
                        owner,
                        target,
                        physical: physical(fact.span())?,
                        spelling: content_hash(written.as_bytes()),
                    },
                );
            }
        }
        Ok(result)
    }

    pub(super) fn admits_parameter(&self, fact: &SyntaxFact) -> bool {
        fact.kind() == SyntaxFactKind::Declaration
            && fact.syntax_kind().as_str() == "php.ordinary_parameter.declaration"
            && self.parameters.contains_key(&fact.local_id())
    }

    /// Join raw definitions to the exact currently materialized native file owner.
    /// Resolver-produced candidate provenance is never an authority for these bindings.
    pub(super) fn targets(
        &self,
        entities: &HashMap<u64, MaterializedEntity>,
        file: &FileRecord,
        source: &SourceRef,
        native: FactId,
        cancel: &Cancellation,
    ) -> Result<Targets, AdapterError> {
        cancel.check()?;
        let mut result = Targets {
            source: source.clone(),
            native,
            references: HashMap::new(),
            calls: HashMap::new(),
        };
        if file.id != source.span().file()
            || file.repository != source.repository()
            || file.generation != source.generation()
            || file.provenance != native
            || file.language != "php"
            || file.content_hash != source.content_hash()
            || file.byte_length != source.span().end_byte()
            || file.evidence.source.as_ref() != Some(source)
        {
            return Ok(result);
        }
        let mut admitted = HashMap::new();
        admitted
            .try_reserve(
                self.functions
                    .len()
                    .checked_add(self.parameters.len())
                    .ok_or(SinkError::AllocationFailed)?,
            )
            .map_err(|_| SinkError::AllocationFailed)?;
        for raw in self.functions.values() {
            cancel.check()?;
            let Some(entity) = entities.get(&raw.local) else {
                continue;
            };
            if valid_entity(
                entity,
                raw,
                source,
                native,
                EntityKind::Function,
                ContainerRef::File(file.id),
            ) {
                admitted.insert(raw.local, entity.record.id);
            }
        }
        for raw in self.parameters.values() {
            cancel.check()?;
            let Some(owner) = raw.owner.and_then(|id| admitted.get(&id)).copied() else {
                continue;
            };
            let Some(entity) = entities.get(&raw.local) else {
                continue;
            };
            if valid_entity(
                entity,
                raw,
                source,
                native,
                EntityKind::Parameter,
                ContainerRef::Entity(owner),
            ) {
                admitted.insert(raw.local, entity.record.id);
            }
        }
        for (raws, output) in [
            (&self.references, &mut result.references),
            (&self.calls, &mut result.calls),
        ] {
            output
                .try_reserve(raws.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            for (local, raw) in raws {
                cancel.check()?;
                let Some(owner) = admitted.get(&raw.owner).copied() else {
                    continue;
                };
                let Some(target) = admitted.get(&raw.target).copied() else {
                    continue;
                };
                output.insert(
                    *local,
                    Binding {
                        owner,
                        target,
                        physical: raw.physical,
                        spelling: raw.spelling,
                    },
                );
            }
        }
        Ok(result)
    }
}

fn valid_entity(
    entity: &MaterializedEntity,
    raw: &RawEntity,
    source: &SourceRef,
    native: FactId,
    kind: EntityKind,
    parent: ContainerRef,
) -> bool {
    let (tag, identity): (u8, &[u8]) = match &parent {
        ContainerRef::Repository(_) => return false,
        ContainerRef::File(file) => (1, file.as_bytes()),
        ContainerRef::Entity(owner) => (2, owner.as_bytes()),
    };
    entity.record.kind == kind
        && entity.record.language == "php"
        && entity.record.repository == source.repository()
        && entity.record.generation == source.generation()
        && entity.record.provenance == native
        && entity.record.canonical_name == raw.name
        && entity.direct_parent == parent
        && entity.record.container == Some(parent)
        && entity.identity_claim.container == Some(parent)
        && entity.identity_claim.kind == kind
        && entity.identity_claim.repository == source.repository()
        && entity.identity_claim.language == "php"
        && entity.identity_claim.declared_identity == raw.name
        && entity.identity_claim.signature_discriminator == raw.signature
        && entity.identity_claim.container_identity.first() == Some(&tag)
        && entity.identity_claim.container_identity.get(1..) == Some(identity)
        && entity.identity_claim.symbol == entity.record.id
        && entity.identity_claim.derived_symbol() == entity.record.id
        && entity.definition_local_id == Some(raw.definition)
        && entity.signature_span == raw.header
        && entity.record.evidence.source.as_ref() == Some(&source_for_span(source, raw.physical))
}

struct Binding {
    owner: SymbolId,
    target: SymbolId,
    physical: SourceSpan,
    spelling: rootlight_ids::ContentHash,
}

pub(super) struct Targets {
    source: SourceRef,
    native: FactId,
    references: HashMap<u64, Binding>,
    calls: HashMap<u64, Binding>,
}

impl Targets {
    fn permits(&self, binding: &Binding, occurrence: &OccurrenceRecord, text: &str) -> bool {
        occurrence.provenance == self.native
            && occurrence.repository == self.source.repository()
            && occurrence.generation == self.source.generation()
            && occurrence.file == self.source.span().file()
            && occurrence.source == source_for_span(&self.source, binding.physical)
            && occurrence.enclosing == Some(binding.owner)
            && occurrence.role == OccurrenceRole::Reference
            && occurrence.syntactic_text_hash == binding.spelling
            && binding.spelling == content_hash(text.as_bytes())
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    }

    pub(super) fn formal(
        &self,
        fact: &SyntaxFact,
        occurrence: &OccurrenceRecord,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.references.get(&fact.local_id()) else {
            return Ok(None);
        };
        Ok(
            (fact.syntax_kind().as_str() == "php.ordinary_formal.reference"
                && occurrence.syntax_kind == fact.syntax_kind().as_str()
                && self.permits(binding, occurrence, text))
            .then_some(binding.target),
        )
    }

    pub(super) fn call(
        &self,
        fact: &SyntaxFact,
        occurrence: &OccurrenceRecord,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<(OccurrenceRecord, RelationRecord)>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.calls.get(&fact.local_id()) else {
            return Ok(None);
        };
        if fact.syntax_kind().as_str() != "php.name.reference"
            || occurrence.syntax_kind != "php.name.reference"
            || !self.permits(binding, occurrence, text)
        {
            return Ok(None);
        }
        let mut call = occurrence.clone();
        call.role = OccurrenceRole::CallSite;
        call.syntax_kind.clear();
        call.syntax_kind
            .try_reserve(CALL.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        call.syntax_kind.push_str(CALL);
        call.target = OccurrenceTarget::Resolved {
            symbol: binding.target,
        };
        call.id = rootlight_ir::derive_occurrence_record_id(&call)
            .map_err(|_| provider_failure("php-ordinary-call-identity"))?;
        let mut edge = lexical_reference_relation(&call, binding.target)?;
        edge.subject = RelationEndpoint::Entity(binding.owner);
        edge.predicate = RelationPredicate::Calls;
        edge.evidence
            .derivation
            .try_reserve_exact(3)
            .map_err(|_| SinkError::AllocationFailed)?;
        edge.evidence.derivation.extend([
            rootlight_ir::FactRef::Entity(binding.owner),
            rootlight_ir::FactRef::Fact(call.id),
            rootlight_ir::FactRef::Entity(binding.target),
        ]);
        edge.evidence.derivation.sort_unstable();
        edge.evidence.derivation.dedup();
        edge.id = rootlight_ir::derive_relation_record_id(&edge)
            .map_err(|_| provider_failure("php-ordinary-call-relation-identity"))?;
        Ok(Some((call, edge)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, GenerationId, RepositoryId};
    use rootlight_ir::{
        AnalysisTier, EntityRecord, EntityVisibility, FactEvidence, SymbolIdentityClaim,
    };

    fn source() -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(FileId::from_bytes([3; 20]), 0, 64).unwrap(),
            content_hash(&[b'x'; 64]),
            None,
        )
    }

    fn member(kind: EntityKind, parent: ContainerRef) -> (MaterializedEntity, RawEntity) {
        let source = source();
        let mut container_identity = Vec::new();
        match parent {
            ContainerRef::Repository(repository) => {
                container_identity.push(0);
                container_identity.extend_from_slice(repository.as_bytes());
            }
            ContainerRef::File(file) => {
                container_identity.push(1);
                container_identity.extend_from_slice(file.as_bytes());
            }
            ContainerRef::Entity(owner) => {
                container_identity.push(2);
                container_identity.extend_from_slice(owner.as_bytes());
            }
        }
        let raw = RawEntity {
            local: 1,
            definition: 2,
            name: if kind == EntityKind::Parameter {
                "$lat"
            } else {
                "owned"
            }
            .to_owned(),
            physical: SourceSpan::new(source.span().file(), 6, 12).unwrap(),
            header: None,
            signature: Vec::new(),
            owner: None,
        };
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: source.repository(),
            language: "php".to_owned(),
            kind,
            container: Some(parent),
            container_identity,
            declared_identity: raw.name.clone(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: Vec::new(),
        };
        claim.symbol = claim.derived_symbol();
        let member = MaterializedEntity {
            record: EntityRecord {
                id: claim.symbol,
                repository: source.repository(),
                generation: source.generation(),
                kind,
                language: "php".to_owned(),
                tier: AnalysisTier::TierD,
                canonical_name: raw.name.clone(),
                display_name: raw.name.clone(),
                qualified_name: raw.name.clone(),
                container: Some(parent),
                visibility: EntityVisibility::Unknown,
                flags: Vec::new(),
                provenance: FactId::from_bytes([4; 20]),
                evidence: FactEvidence {
                    source: Some(source_for_span(&source, raw.physical)),
                    derivation: Vec::new(),
                },
            },
            identity_claim: claim,
            direct_parent: parent,
            identity_guard: [0; 32],
            definition_local_id: Some(raw.definition),
            signature_evidence: None,
            signature_span: None,
        };
        (member, raw)
    }

    #[test]
    fn materialized_owner_definition_identity_header_and_native_provenance_are_independent_joins() {
        let source = source();
        let native = FactId::from_bytes([4; 20]);
        for (kind, parent) in [
            (
                EntityKind::Function,
                ContainerRef::File(source.span().file()),
            ),
            (
                EntityKind::Parameter,
                ContainerRef::Entity(SymbolId::from_bytes([5; 20])),
            ),
        ] {
            let (valid, raw) = member(kind, parent);
            assert!(valid_entity(&valid, &raw, &source, native, kind, parent));
            for mode in 0..11 {
                let mut changed = member(kind, parent).0;
                match mode {
                    0 => changed.record.provenance = FactId::from_bytes([6; 20]),
                    1 => {
                        changed.direct_parent = ContainerRef::Entity(SymbolId::from_bytes([7; 20]))
                    }
                    2 => changed.record.container = None,
                    3 => changed.identity_claim.container = None,
                    4 => changed.identity_claim.container_identity.push(3),
                    5 => changed.identity_claim.declared_identity.push('x'),
                    6 => changed.definition_local_id = Some(99),
                    7 => changed.signature_span = Some(source.span()),
                    8 => changed.identity_claim.signature_discriminator.push(b'x'),
                    9 => changed
                        .record
                        .evidence
                        .source
                        .as_mut()
                        .unwrap()
                        .clone_from(&source),
                    _ => changed.record.id = SymbolId::from_bytes([8; 20]),
                }
                assert!(
                    !valid_entity(&changed, &raw, &source, native, kind, parent),
                    "mode {mode}"
                );
            }
        }
        let parent = ContainerRef::Repository(source.repository());
        for kind in [EntityKind::Function, EntityKind::Parameter] {
            let (entity, raw) = member(kind, parent);
            assert!(!valid_entity(&entity, &raw, &source, native, kind, parent));
        }
    }

    #[test]
    fn independent_cst_census_refuses_formal_writes_aliases_and_its_own_one_less_work() {
        let good = b"<?php function owned($lat) { return $lat; }";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .unwrap();
        let tree = parser.parse(good, None).unwrap();
        let node = tree.root_node().named_child(1).unwrap();
        assert_eq!(node.kind(), "function_definition");
        let mut measured = Work { remaining: 4096 };
        assert!(
            function(good, node, &mut measured, &Cancellation::new())
                .unwrap()
                .unwrap()
                .admitted
        );
        let exact = 4096 - measured.remaining;
        assert!(
            function(
                good,
                node,
                &mut Work { remaining: exact },
                &Cancellation::new()
            )
            .unwrap()
            .unwrap()
            .admitted
        );
        assert!(
            function(
                good,
                node,
                &mut Work {
                    remaining: exact - 1
                },
                &Cancellation::new()
            )
            .is_err()
        );
        for source in [
            "<?php function owned($lat) { $lat++; return $lat; }",
            "<?php function owned($lat) { $alias =& $lat; return $lat; }",
            "<?php function owned($lat) { foreach ([] as $lat) {} return $lat; }",
        ] {
            let tree = parser.parse(source, None).unwrap();
            assert!(!tree.root_node().has_error());
            let node = tree.root_node().named_child(1).unwrap();
            assert!(
                !function(
                    source.as_bytes(),
                    node,
                    &mut Work { remaining: 4096 },
                    &Cancellation::new()
                )
                .unwrap()
                .unwrap()
                .admitted
            );
        }
    }
}
