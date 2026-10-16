//! Native Julia owners preserve physical names, method headers and containment.
//! Quoted data is excluded; macro operands remain written syntax with explicit gaps.

use super::{QueryCandidate, StructuralRole, native_depth};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use std::{collections::HashMap, ops::Range};
use tree_sitter::Node;

#[derive(Clone)]
struct Owner {
    name: Option<Range<usize>>,
    header: Range<usize>,
    syntax: &'static str,
}

pub(super) struct CaptureCache {
    owners: HashMap<usize, Option<Owner>>,
    visibility: HashMap<usize, bool>,
    remaining: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            owners: HashMap::new(),
            visibility: HashMap::new(),
            remaining: limit,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }

    fn visible(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = node.parent();
        let mut pending = Vec::new();
        let mut visible = true;
        while let Some(parent) = current {
            if let Some(found) = self.visibility.get(&parent.id()) {
                visible = *found;
                break;
            }
            if !self.visit(cancellation)? {
                return Ok(false);
            }
            pending
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.push(parent.id());
            if matches!(
                parent.kind(),
                "quote_statement"
                    | "quote_expression"
                    | "string_literal"
                    | "command_literal"
                    | "prefixed_string_literal"
                    | "prefixed_command_literal"
                    | "line_comment"
                    | "block_comment"
                    | "ERROR"
            ) {
                visible = false;
                break;
            }
            current = parent.parent();
        }
        self.visibility
            .try_reserve(pending.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for id in pending {
            self.visibility.insert(id, visible);
        }
        Ok(visible)
    }

    fn owner(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Owner>, AdapterError> {
        if let Some(owner) = self.owners.get(&node.id()) {
            return Ok(owner.clone());
        }
        if !self.visit(cancellation)? {
            return Ok(None);
        }
        let owner = owner(node, source, cancellation, self)?;
        self.owners
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.owners.insert(node.id(), owner.clone());
        Ok(owner)
    }
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn quoted_identifier(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "prefixed_string_literal"
        && clean(node, source)
        && node
            .child_by_field_name("prefix")
            .is_some_and(|prefix| source.get(prefix.byte_range()) == Some(b"var"))
        && node.child_by_field_name("suffix").is_none()
        && source.get(node.byte_range()).is_some_and(|text| {
            text.starts_with(b"var\"") && !text.starts_with(b"var\"\"\"") && text.ends_with(b"\"")
        })
}

fn child<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for candidate in node.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if candidate.kind() == kind {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn unwrap_head<'tree>(
    mut node: Node<'tree>,
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    while matches!(
        node.kind(),
        "signature"
            | "type_head"
            | "where_expression"
            | "typed_expression"
            | "parametrized_type_expression"
            | "parenthesized_expression"
    ) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        let Some(first) = node.named_child(0) else {
            return Ok(None);
        };
        node = first;
    }
    Ok(Some(node))
}

fn name(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Range<usize>>, AdapterError> {
    if !clean(node, source) {
        return Ok(None);
    }
    match node.kind() {
        "identifier" | "operator" => Ok(Some(node.byte_range())),
        "prefixed_string_literal" if quoted_identifier(node, source) => Ok(Some(node.byte_range())),
        "parametrized_type_expression" => {
            let Some(head) = node.named_child(0) else {
                return Ok(None);
            };
            if !cache.visit(cancellation)? {
                return Ok(None);
            }
            name(head, source, cancellation, cache)
        }
        "field_expression" => {
            let mut cursor = node.walk();
            let mut leaf = None;
            for part in node.named_children(&mut cursor) {
                if !cache.visit(cancellation)? {
                    return Ok(None);
                }
                let Some(part) = name(part, source, cancellation, cache)? else {
                    return Ok(None);
                };
                leaf = Some(part);
            }
            Ok(leaf)
        }
        "quote_expression" | "parenthesized_expression" if node.named_child_count() == 1 => {
            let Some(inner) = node.named_child(0) else {
                return Ok(None);
            };
            if !cache.visit(cancellation)? {
                return Ok(None);
            }
            name(inner, source, cancellation, cache)
        }
        _ => Ok(None),
    }
}

fn function_head<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let Some(head) = unwrap_head(node, cancellation, cache)? else {
        return Ok(None);
    };
    Ok((head.kind() == "call_expression").then_some(head))
}

fn anonymous_header(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Range<usize>>, AdapterError> {
    let header = match node.kind() {
        "arrow_function_expression" => {
            if node.named_child_count() != 2
                || node.named_child(1).is_none_or(|body| body.is_missing())
            {
                return Ok(None);
            }
            let Some(first) = node.named_child(0) else {
                return Ok(None);
            };
            first
        }
        "function_definition" => {
            let Some(signature) = child(node, "signature", cancellation, cache)? else {
                return Ok(None);
            };
            let mut head = signature;
            while matches!(
                head.kind(),
                "signature" | "where_expression" | "typed_expression"
            ) {
                if !cache.visit(cancellation)? {
                    return Ok(None);
                }
                let Some(first) = head.named_child(0) else {
                    return Ok(None);
                };
                head = first;
            }
            if !matches!(head.kind(), "argument_list" | "parenthesized_expression") {
                return Ok(None);
            }
            signature
        }
        "do_clause" => {
            let Some(block) = child(node, "block", cancellation, cache)? else {
                return Ok(None);
            };
            let mut end = node.start_byte();
            let mut cursor = node.walk();
            for part in node.children(&mut cursor) {
                if !cache.visit(cancellation)? {
                    return Ok(None);
                }
                if part == block {
                    break;
                }
                if !clean(part, source) {
                    return Ok(None);
                }
                end = part.end_byte();
            }
            return Ok(Some(node.start_byte()..end));
        }
        _ => return Ok(None),
    };
    Ok(clean(header, source).then(|| node.start_byte()..header.end_byte()))
}

fn anonymous_parameter_context(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let range = node.byte_range();
    let mut current = node;
    while let Some(parent) = current.parent() {
        if !cache.visit(cancellation)? {
            return Ok(false);
        }
        match parent.kind() {
            "tuple_expression" | "open_tuple" | "argument_list" => {}
            "typed_expression"
            | "splat_expression"
            | "parenthesized_expression"
            | "named_argument"
            | "signature"
            | "where_expression"
                if parent.named_child(0) == Some(current) => {}
            "arrow_function_expression" | "function_definition" | "do_clause" => {
                return Ok(cache
                    .owner(parent, source, cancellation)?
                    .is_some_and(|owner| {
                        owner.syntax == "julia.anonymous_function"
                            && owner.header.start <= range.start
                            && owner.header.end >= range.end
                    }));
            }
            _ => return Ok(false),
        }
        current = parent;
    }
    Ok(false)
}

fn parameter_owner<'tree>(
    list: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if anonymous_parameter_context(list, source, cancellation, cache)? {
        return Ok(list.parent());
    }
    let Some(call) = list
        .parent()
        .filter(|node| node.kind() == "call_expression")
    else {
        return Ok(None);
    };
    let mut current = call.parent();
    while let Some(parent) = current {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        match parent.kind() {
            "where_expression" | "typed_expression" | "signature" | "parenthesized_expression" => {
                current = parent.parent()
            }
            "function_definition" | "macro_definition" | "assignment" => {
                let proof = cache.owner(parent, source, cancellation)?;
                return Ok(proof
                    .filter(|proof| {
                        matches!(
                            proof.syntax,
                            "julia.function"
                                | "julia.macro"
                                | "julia.callable_object"
                                | "julia.anonymous_function"
                        ) && proof.header.end >= call.end_byte()
                    })
                    .map(|_| parent));
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn tuple_component(node: Node<'_>) -> bool {
    node.parent()
        .is_some_and(|parent| matches!(parent.kind(), "tuple_expression" | "open_tuple"))
}

fn type_parameter_component(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "curly_expression"
            || parent.kind() == "where_expression" && parent.named_child(1) == Some(node)
    })
}

fn method_type_scope(
    mut node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let range = node.byte_range();
    while let Some(parent) = node.parent() {
        if !cache.visit(cancellation)? {
            return Ok(false);
        }
        match parent.kind() {
            "where_expression" | "typed_expression" | "signature" | "parenthesized_expression"
                if parent.named_child(0) == Some(node) =>
            {
                node = parent
            }
            "function_definition" | "macro_definition" | "assignment" => {
                return Ok(cache
                    .owner(parent, source, cancellation)?
                    .is_some_and(|owner| {
                        matches!(
                            owner.syntax,
                            "julia.function"
                                | "julia.macro"
                                | "julia.callable_object"
                                | "julia.anonymous_function"
                        ) && owner.header.start <= range.start
                            && owner.header.end >= range.end
                    }));
            }
            _ => return Ok(false),
        }
    }
    Ok(false)
}

fn type_parameter_context(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let Some(parent) = node.parent() else {
        return Ok(false);
    };
    if parent.kind() == "where_expression" && parent.named_child(1) == Some(node) {
        return if node.kind() == "curly_expression" {
            Ok(false)
        } else {
            method_type_scope(parent, source, cancellation, cache)
        };
    }
    if parent.kind() != "curly_expression" {
        return Ok(false);
    }
    let Some(mut head) = parent.parent() else {
        return Ok(false);
    };
    if head.kind() == "where_expression" && head.named_child(1) == Some(parent) {
        return method_type_scope(head, source, cancellation, cache);
    }
    if head.kind() != "parametrized_type_expression" {
        return Ok(false);
    }
    // Supertype arguments reuse bindings. Only the left declared type head
    // introduces parameters, even when both sides contain identical names.
    while let Some(parent) = head.parent() {
        if !cache.visit(cancellation)? {
            return Ok(false);
        }
        match parent.kind() {
            "binary_expression" | "type_head" if parent.named_child(0) == Some(head) => {
                head = parent
            }
            "struct_definition" | "abstract_definition" | "primitive_definition" => {
                return Ok(cache.owner(parent, source, cancellation)?.is_some());
            }
            _ => return Ok(false),
        }
    }
    Ok(false)
}

fn type_parameter_name(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Range<usize>>, AdapterError> {
    if !clean(node, source) {
        return Ok(None);
    }
    let head = if node.kind() == "binary_expression" && node.named_child_count() == 3 {
        let Some(operator) = node.named_child(1) else {
            return Ok(None);
        };
        if !matches!(source.get(operator.byte_range()), Some(b"<:" | b">:")) {
            return Ok(None);
        }
        let Some(left) = node.named_child(0) else {
            return Ok(None);
        };
        if left.kind() == "binary_expression" {
            if left.named_child_count() != 3
                || !left.named_child(1).is_some_and(|inner_operator| {
                    source.get(inner_operator.byte_range()) == source.get(operator.byte_range())
                })
                || left.named_child(0).is_some_and(|bound| {
                    bound.kind() == "binary_expression"
                        && bound.named_child(1).is_some_and(|bound_operator| {
                            source.get(bound_operator.byte_range())
                                == source.get(operator.byte_range())
                        })
                })
            {
                return Ok(None);
            }
            // A two-sided bound declares the middle name; both endpoints are uses.
            left.named_child(2)
        } else {
            Some(left)
        }
    } else {
        Some(node)
    };
    let Some(head) =
        head.filter(|head| head.kind() == "identifier" || quoted_identifier(*head, source))
    else {
        return Ok(None);
    };
    binding_name(head, source, cancellation, cache)
}

fn explicit_header_component(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(parent.kind(), "local_statement" | "global_statement")
            || parent.kind() == "let_statement"
                && !matches!(node.kind(), "block" | "line_comment" | "block_comment")
    })
}

fn variable_syntax(node: Node<'_>) -> &'static str {
    if node
        .parent()
        .is_some_and(|parent| parent.kind() == "global_statement")
    {
        "julia.global_variable"
    } else {
        "julia.variable"
    }
}

fn outer_binding(node: Node<'_>) -> bool {
    node.kind() == "for_binding" && node.child(0).is_some_and(|first| first.kind() == "outer")
}

fn single_pattern_wrapper(node: Node<'_>) -> bool {
    matches!(
        (node.kind(), node.named_child_count()),
        ("typed_expression", 2) | ("splat_expression" | "parenthesized_expression", 1)
    )
}

fn pattern_context(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<&'static str>, AdapterError> {
    if anonymous_parameter_context(node, source, cancellation, cache)? {
        return Ok(Some("julia.parameter"));
    }
    let mut current = node;
    let mut tuple = matches!(node.kind(), "tuple_expression" | "open_tuple");
    while let Some(parent) = current.parent() {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        match parent.kind() {
            "tuple_expression" | "open_tuple" => tuple = true,
            "typed_expression" | "splat_expression" | "parenthesized_expression"
                if single_pattern_wrapper(parent) && parent.named_child(0) == Some(current) => {}
            "assignment" if tuple && parent.named_child(0) == Some(current) => {
                return Ok(Some(variable_syntax(parent)));
            }
            "for_binding" if parent.named_child(0) == Some(current) && !outer_binding(parent) => {
                return Ok(Some("julia.variable"));
            }
            "local_statement" => return Ok(Some("julia.variable")),
            "global_statement" => return Ok(Some("julia.global_variable")),
            "let_statement" if current.kind() == "identifier" => return Ok(Some("julia.variable")),
            "argument_list" if tuple => {
                return Ok(parameter_owner(parent, source, cancellation, cache)?
                    .map(|_| "julia.parameter"));
            }
            _ => return Ok(None),
        }
        current = parent;
    }
    Ok(None)
}

fn complete_pattern(
    node: Node<'_>,
    syntax: &str,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut pending = Vec::new();
    pending
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    pending.push(node);
    while let Some(part) = pending.pop() {
        if !cache.visit(cancellation)? || !clean(part, source) {
            return Ok(false);
        }
        match part.kind() {
            "identifier" => {}
            "prefixed_string_literal" if quoted_identifier(part, source) => {}
            "unary_typed_expression" if syntax == "julia.parameter" => {}
            // These targets mutate existing storage and introduce no named binding.
            "field_expression" | "index_expression" if syntax == "julia.variable" => {}
            "typed_expression" | "splat_expression" | "parenthesized_expression"
                if single_pattern_wrapper(part) =>
            {
                let Some(first) = part.named_child(0) else {
                    return Ok(false);
                };
                pending
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                pending.push(first);
            }
            "tuple_expression" | "open_tuple" | "argument_list" => {
                pending
                    .try_reserve(part.named_child_count())
                    .map_err(|_| SinkError::AllocationFailed)?;
                let mut cursor = part.walk();
                pending.extend(part.named_children(&mut cursor));
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn covered_pattern(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut node = if matches!(node.kind(), "assignment" | "for_binding") {
        let Some(lhs) = node.named_child(0) else {
            return Ok(false);
        };
        lhs
    } else {
        node
    };
    while single_pattern_wrapper(node) {
        if !cache.visit(cancellation)? {
            return Ok(false);
        }
        let Some(first) = node.named_child(0) else {
            return Ok(false);
        };
        node = first;
    }
    let Some(syntax) = pattern_context(node, source, cancellation, cache)? else {
        return Ok(false);
    };
    complete_pattern(node, syntax, source, cancellation, cache)
}

fn binding_name(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Range<usize>>, AdapterError> {
    if !clean(node, source) {
        return Ok(None);
    }
    let mut head = node;
    while single_pattern_wrapper(head) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        let Some(first) = head.named_child(0) else {
            return Ok(None);
        };
        head = first;
    }
    Ok(
        ((head.kind() == "identifier" || quoted_identifier(head, source))
            && source
                .get(head.byte_range())
                .is_some_and(|text| !text.iter().all(|byte| *byte == b'_')))
        .then(|| head.byte_range()),
    )
}

fn unnamed_parameter(node: Node<'_>, source: &[u8]) -> Option<Owner> {
    (node.kind() == "unary_typed_expression" && clean(node, source)).then(|| Owner {
        name: None,
        header: node.byte_range(),
        syntax: "julia.unnamed_parameter",
    })
}

fn mutation_target(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    if !matches!(node.kind(), "assignment" | "compound_assignment_expression")
        || !clean(node, source)
    {
        return Ok(false);
    }
    let Some(mut target) = node.named_child(0) else {
        return Ok(false);
    };
    while single_pattern_wrapper(target) {
        if !cache.visit(cancellation)? {
            return Ok(false);
        }
        let Some(first) = target.named_child(0) else {
            return Ok(false);
        };
        target = first;
    }
    let storage = matches!(target.kind(), "field_expression" | "index_expression");
    if node.kind() == "assignment" {
        return Ok(storage);
    }
    let Some(operator) = node.named_child(1).filter(|part| part.kind() == "operator") else {
        return Ok(false);
    };
    let Some(text) = source
        .get(operator.byte_range())
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
    else {
        return Ok(false);
    };
    if text == ".=" {
        return Ok(true);
    }
    let (broadcast, operator) = text
        .strip_prefix('.')
        .map_or((false, text), |operator| (true, operator));
    // Other native compound forms can be macro syntax rather than storage writes.
    Ok((storage || broadcast)
        && matches!(
            operator,
            "+=" | "-="
                | "−="
                | "*="
                | "/="
                | "//="
                | "\\="
                | "^="
                | "%="
                | "<<="
                | ">>="
                | ">>>="
                | "|="
                | "&="
                | "÷="
                | "⊻="
        ))
}

fn owner(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Owner>, AdapterError> {
    if node.is_missing() || node.is_error() || source.get(node.byte_range()).is_none() {
        return Ok(None);
    }
    // Recovery in a body does not erase an independently parsed written header.
    // A missing closing token cannot establish the complete declaration owner.
    if matches!(
        node.kind(),
        "module_definition"
            | "function_definition"
            | "macro_definition"
            | "struct_definition"
            | "abstract_definition"
            | "primitive_definition"
            | "do_clause"
    ) && !node
        .child_count()
        .checked_sub(1)
        .and_then(|index| u32::try_from(index).ok())
        .and_then(|index| node.child(index))
        .is_some_and(|last| last.kind() == "end" && !last.is_missing())
    {
        return Ok(None);
    }
    if let Some(header) = anonymous_header(node, source, cancellation, cache)? {
        return Ok(Some(Owner {
            name: None,
            header,
            syntax: "julia.anonymous_function",
        }));
    }
    if anonymous_parameter_context(node, source, cancellation, cache)? {
        if let Some(parameter) = unnamed_parameter(node, source) {
            return Ok(Some(parameter));
        }
        if let Some(name) = binding_name(node, source, cancellation, cache)? {
            return Ok(Some(Owner {
                name: Some(name),
                header: node.byte_range(),
                syntax: "julia.parameter",
            }));
        }
    }
    if node.kind() == "for_binding" {
        if outer_binding(node) {
            return Ok(None);
        }
        let Some(lhs) = node.named_child(0) else {
            return Ok(None);
        };
        return Ok(
            binding_name(lhs, source, cancellation, cache)?.map(|name| Owner {
                name: Some(name),
                header: lhs.byte_range(),
                syntax: "julia.variable",
            }),
        );
    }
    if type_parameter_component(node) {
        if !type_parameter_context(node, source, cancellation, cache)? {
            return Ok(None);
        }
        return Ok(
            type_parameter_name(node, source, cancellation, cache)?.map(|name| Owner {
                name: Some(name),
                header: node.byte_range(),
                syntax: "julia.type_parameter",
            }),
        );
    }
    if explicit_header_component(node)
        && (matches!(node.kind(), "identifier" | "typed_expression")
            || quoted_identifier(node, source))
    {
        return Ok(
            binding_name(node, source, cancellation, cache)?.map(|name| Owner {
                name: Some(name),
                header: node.byte_range(),
                syntax: variable_syntax(node),
            }),
        );
    }
    if tuple_component(node) {
        let Some(syntax) = pattern_context(node, source, cancellation, cache)? else {
            return Ok(None);
        };
        return Ok(
            binding_name(node, source, cancellation, cache)?.map(|name| Owner {
                name: Some(name),
                header: node.byte_range(),
                syntax,
            }),
        );
    }
    if matches!(node.kind(), "typed_expression" | "unary_typed_expression")
        && let Some(wrapper) = node
            .parent()
            .filter(|parent| parent.kind() == "parenthesized_expression")
        && let Some(call) = wrapper.parent().filter(|parent| {
            parent.kind() == "call_expression" && parent.named_child(0) == Some(wrapper)
        })
        && let Some(list) = child(call, "argument_list", cancellation, cache)?
        && parameter_owner(list, source, cancellation, cache)?.is_some()
    {
        if let Some(parameter) = unnamed_parameter(node, source) {
            return Ok(Some(parameter));
        }
        let Some(head) = node.named_child(0) else {
            return Ok(None);
        };
        return Ok(name(head, source, cancellation, cache)?.map(|name| Owner {
            name: Some(name),
            header: node.byte_range(),
            syntax: "julia.parameter",
        }));
    }
    if let Some(list) = node
        .parent()
        .filter(|parent| parent.kind() == "argument_list")
    {
        if parameter_owner(list, source, cancellation, cache)?.is_none() {
            return Ok(None);
        }
        if let Some(parameter) = unnamed_parameter(node, source) {
            return Ok(Some(parameter));
        }
        let mut head = node;
        while matches!(
            head.kind(),
            "typed_expression" | "named_argument" | "assignment" | "splat_expression"
        ) {
            if !cache.visit(cancellation)? {
                return Ok(None);
            }
            let Some(first) = head.named_child(0) else {
                return Ok(None);
            };
            head = first;
        }
        return Ok(name(head, source, cancellation, cache)?.map(|name| Owner {
            name: Some(name),
            header: node.byte_range(),
            syntax: "julia.parameter",
        }));
    }
    if node
        .parent()
        .filter(|parent| parent.kind() == "block")
        .and_then(|parent| parent.parent())
        .is_some_and(|parent| parent.kind() == "struct_definition")
        && (matches!(
            node.kind(),
            "identifier" | "typed_expression" | "const_statement"
        ) || quoted_identifier(node, source))
    {
        let head = if node.kind() == "const_statement" {
            node.named_child(0)
        } else {
            Some(node)
        };
        let Some(head) = head else {
            return Ok(None);
        };
        let Some(head) = unwrap_head(head, cancellation, cache)? else {
            return Ok(None);
        };
        return Ok(name(head, source, cancellation, cache)?.map(|name| Owner {
            name: Some(name),
            header: node.byte_range(),
            syntax: "julia.field",
        }));
    }
    let (header, head, syntax) = match node.kind() {
        "module_definition" => {
            let Some(head) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            (node.start_byte()..head.end_byte(), head, "julia.module")
        }
        "struct_definition" | "abstract_definition" | "primitive_definition" => {
            let Some(header) = child(node, "type_head", cancellation, cache)? else {
                return Ok(None);
            };
            if !clean(header, source) {
                return Ok(None);
            }
            let Some(mut head) = unwrap_head(header, cancellation, cache)? else {
                return Ok(None);
            };
            if head.kind() == "binary_expression" {
                let Some(first) = head.named_child(0) else {
                    return Ok(None);
                };
                let Some(first) = unwrap_head(first, cancellation, cache)? else {
                    return Ok(None);
                };
                head = first;
            }
            let syntax = match node.kind() {
                "struct_definition" => "julia.struct",
                "abstract_definition" => "julia.abstract",
                _ => "julia.primitive",
            };
            (node.start_byte()..header.end_byte(), head, syntax)
        }
        "function_definition" | "macro_definition" => {
            let Some(header) = child(node, "signature", cancellation, cache)? else {
                return Ok(None);
            };
            if !clean(header, source) {
                return Ok(None);
            }
            let Some(head) = unwrap_head(header, cancellation, cache)? else {
                return Ok(None);
            };
            let head = if head.kind() == "call_expression" {
                head.named_child(0)
            } else {
                Some(head)
            };
            let Some(head) = head else {
                return Ok(None);
            };
            if head.kind() == "parenthesized_expression"
                && head.named_child(0).is_some_and(|child| {
                    matches!(child.kind(), "typed_expression" | "unary_typed_expression")
                })
            {
                return Ok(Some(Owner {
                    name: None,
                    header: node.start_byte()..header.end_byte(),
                    syntax: "julia.callable_object",
                }));
            }
            (
                node.start_byte()..header.end_byte(),
                head,
                if node.kind() == "macro_definition" {
                    "julia.macro"
                } else {
                    "julia.function"
                },
            )
        }
        "assignment" => {
            let Some(lhs) = node.named_child(0) else {
                return Ok(None);
            };
            if !clean(lhs, source) {
                return Ok(None);
            }
            if let Some(call) = function_head(lhs, cancellation, cache)? {
                let Some(head) = call.named_child(0) else {
                    return Ok(None);
                };
                if head.kind() == "parenthesized_expression"
                    && head.named_child_count() == 1
                    && head.named_child(0).is_some_and(|child| {
                        matches!(child.kind(), "typed_expression" | "unary_typed_expression")
                    })
                {
                    return Ok(Some(Owner {
                        name: None,
                        header: lhs.byte_range(),
                        syntax: "julia.callable_object",
                    }));
                }
                (lhs.byte_range(), head, "julia.function")
            } else {
                let Some(head) = unwrap_head(lhs, cancellation, cache)? else {
                    return Ok(None);
                };
                // A dotted assignment may mutate an object property; its final
                // name does not prove a newly introduced Julia binding.
                if !matches!(head.kind(), "identifier" | "operator")
                    && !quoted_identifier(head, source)
                {
                    return Ok(None);
                }
                (lhs.byte_range(), head, variable_syntax(node))
            }
        }
        _ => return Ok(None),
    };
    Ok(name(head, source, cancellation, cache)?.map(|name| Owner {
        name: Some(name),
        header,
        syntax,
    }))
}

fn in_declaration_head(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
    names_only: bool,
) -> Result<bool, AdapterError> {
    let mut current = Some(node);
    while let Some(parent) = current {
        if !cache.visit(cancellation)? {
            return Ok(true);
        }
        if let Some(owner) = cache.owner(parent, source, cancellation)? {
            let range = if names_only {
                owner.name
            } else {
                Some(owner.header)
            };
            if range.is_some_and(|range| {
                range.start <= node.start_byte() && range.end >= node.end_byte()
            }) {
                return Ok(true);
            }
        }
        if matches!(parent.kind(), "block" | "source_file") {
            break;
        }
        current = parent.parent();
    }
    Ok(false)
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    let mut range = node.byte_range();
    let syntax = if matches!(role, StructuralRole::Root | StructuralRole::Module) {
        range = 0..source.len();
        "julia.file"
    } else if !cache.visible(node, cancellation)?
        || matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
        ) && node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "typed_expression" | "splat_expression" | "parenthesized_expression"
            ) && parent.named_child(0) == Some(node)
        }) && anonymous_parameter_context(node, source, cancellation, cache)?
        || matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
        ) && (tuple_component(node)
            && pattern_context(node, source, cancellation, cache)?.is_none()
            || type_parameter_component(node)
                && !type_parameter_context(node, source, cancellation, cache)?)
    {
        return Ok(None);
    } else if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) && (matches!(
        node.kind(),
        "module_definition"
            | "function_definition"
            | "macro_definition"
            | "abstract_definition"
            | "primitive_definition"
            | "struct_definition"
            | "assignment"
            | "for_binding"
            | "typed_expression"
            | "unary_typed_expression"
            | "arrow_function_expression"
            | "do_clause"
    ) || anonymous_parameter_context(node, source, cancellation, cache)?
        || explicit_header_component(node)
        || tuple_component(node)
        || type_parameter_component(node)
        || node
            .parent()
            .is_some_and(|parent| parent.kind() == "argument_list")
        || node
            .parent()
            .filter(|parent| parent.kind() == "block")
            .and_then(|parent| parent.parent())
            .is_some_and(|parent| parent.kind() == "struct_definition")
            && (matches!(
                node.kind(),
                "identifier" | "typed_expression" | "const_statement"
            ) || quoted_identifier(node, source)))
    {
        if let Some(list) = node
            .parent()
            .filter(|parent| parent.kind() == "argument_list")
            && !matches!(
                node.kind(),
                "arrow_function_expression" | "function_definition" | "do_clause"
            )
            && parameter_owner(list, source, cancellation, cache)?.is_none()
        {
            return Ok(None);
        }
        match cache.owner(node, source, cancellation)? {
            Some(owner) => match role {
                StructuralRole::Declaration => owner.syntax,
                StructuralRole::Definition => {
                    let Some(name) = owner.name else {
                        return Ok(None);
                    };
                    range = name;
                    "julia.name"
                }
                StructuralRole::Signature => {
                    range = owner.header;
                    "julia.written_header"
                }
                _ => return Ok(None),
            },
            None if node
                .parent()
                .is_some_and(|parent| parent.kind() == "parenthesized_expression")
                && !anonymous_parameter_context(node, source, cancellation, cache)? =>
            {
                return Ok(None);
            }
            None if role == StructuralRole::Signature && outer_binding(node) => {
                "julia.outer_binding"
            }
            None if role == StructuralRole::Signature
                && mutation_target(node, source, cancellation, cache)? =>
            {
                "julia.mutation_target"
            }
            None if role == StructuralRole::Signature
                && covered_pattern(node, source, cancellation, cache)? =>
            {
                return Ok(None);
            }
            None if role == StructuralRole::Signature
                && !matches!(
                    node.kind(),
                    "line_comment" | "block_comment" | "string_literal"
                ) =>
            {
                "julia.unsupported_owner"
            }
            _ => return Ok(None),
        }
    } else if role == StructuralRole::Call && clean(node, source) {
        if in_declaration_head(node, source, cancellation, cache, false)? {
            return Ok(None);
        }
        let Some(head) = node.named_child(0) else {
            return Ok(None);
        };
        let Some(name) = name(head, source, cancellation, cache)? else {
            return Ok(None);
        };
        range = name;
        "julia.written_call"
    } else if role == StructuralRole::Reference && clean(node, source) {
        if node.kind() == "prefixed_string_literal" && !quoted_identifier(node, source) {
            return Ok(None);
        }
        if in_declaration_head(node, source, cancellation, cache, true)? {
            return Ok(None);
        }
        "julia.identifier"
    } else if role == StructuralRole::Import && clean(node, source) {
        "julia.written_import"
    } else if role == StructuralRole::Signature {
        match node.kind() {
            "where_expression" => {
                if method_type_scope(node, source, cancellation, cache)? {
                    return Ok(None);
                }
                "julia.type_binding_scope"
            }
            "quote_statement" | "quote_expression" => "julia.quoted_source",
            "macrocall_expression" => "julia.macro_expansion",
            "for_statement" | "let_statement" => "julia.local_scope",
            "global_statement" => "julia.global_scope",
            "compound_assignment_expression"
                if mutation_target(node, source, cancellation, cache)? =>
            {
                "julia.mutation_target"
            }
            "compound_assignment_expression" => "julia.binding_scope",
            _ => return Ok(None),
        }
    } else if role == StructuralRole::Comment && clean(node, source) {
        "julia.comment"
    } else if role == StructuralRole::StringLiteral && clean(node, source) {
        if quoted_identifier(node, source) {
            return Ok(None);
        }
        "julia.string"
    } else {
        return Ok(None);
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: matches!(
            role,
            StructuralRole::Root
                | StructuralRole::Module
                | StructuralRole::Declaration
                | StructuralRole::Definition
                | StructuralRole::Signature
        ),
        native_depth: native_depth(node, cancellation)?,
    }))
}
