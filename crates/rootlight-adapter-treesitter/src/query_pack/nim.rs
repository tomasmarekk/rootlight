//! Nim owners and call sites require direct native fields and evaluated context.
//! Conditional selection, expansion and dynamic heads remain ranged analysis gaps.

use rootlight_adapter_sdk::{AdapterError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

fn leaf(node: Node<'_>) -> Option<Node<'_>> {
    let node = if node.kind() == "exported_symbol" {
        node.named_child(0)?
    } else {
        node
    };
    (node.kind() == "identifier" && !node.is_missing() && !node.has_error()).then_some(node)
}

fn name(node: Node<'_>) -> Option<Node<'_>> {
    let node = if node.kind() == "type_declaration" {
        node.named_child(0)
            .filter(|child| child.kind() == "type_symbol_declaration")?
    } else {
        node
    };
    leaf(node.child_by_field_name("name")?)
}

fn visible(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut parent = Some(node);
    while let Some(current) = parent {
        cancellation.check()?;
        if current.is_missing()
            || current.is_error()
            || matches!(
                current.kind(),
                "macro_declaration"
                    | "template_declaration"
                    | "method_declaration"
                    | "iterator_declaration"
                    | "converter_declaration"
                    | "conditional_declaration"
                    | "when"
                    | "proc_expression"
                    | "func_expression"
                    | "iterator_expression"
                    | "pragma_statement"
                    | "term_rewriting_pattern"
                    | "do_block"
            )
            || current != node && current.kind() == "call"
        {
            return Ok(false);
        }
        parent = current.parent();
    }
    Ok(true)
}

fn inside_block(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut parent = node.parent();
    while let Some(current) = parent {
        cancellation.check()?;
        if current.kind() == "block" {
            return Ok(true);
        }
        parent = current.parent();
    }
    Ok(false)
}

fn declaration<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<(&'static str, Node<'tree>)>, AdapterError> {
    if node.has_error() || !visible(node, cancellation)? || inside_block(node, cancellation)? {
        return Ok(None);
    }
    let syntax = match node.kind() {
        "proc_declaration" | "func_declaration"
            if node.child_by_field_name("rewrite_pattern").is_none() =>
        {
            "nim.function"
        }
        "type_declaration" => match node.named_child(1).map(|child| child.kind()) {
            Some("object_declaration") => "nim.object",
            Some("enum_declaration") => "nim.enum",
            Some(
                "type_expression" | "ref_type" | "pointer_type" | "distinct_type" | "tuple_type",
            ) => "nim.alias",
            _ => return Ok(None),
        },
        "symbol_declaration" => match node
            .parent()
            .and_then(|parent| parent.parent())
            .map(|parent| parent.kind())
        {
            Some("field_declaration") => "nim.field",
            Some("parameter_declaration") => "nim.parameter",
            Some("variable_declaration" | "for") => "nim.variable",
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    Ok(name(node).map(|name| (syntax, name)))
}

fn call_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if node.has_error() || !visible(node, cancellation)? {
        return Ok(None);
    }
    let Some(name) = node.child_by_field_name("function").and_then(leaf) else {
        return Ok(None);
    };
    let mut parent = node.parent();
    while let Some(current) = parent {
        cancellation.check()?;
        if matches!(current.kind(), "proc_declaration" | "func_declaration") {
            // Default parameter values and type expressions are not procedure bodies.
            return Ok(current
                .child_by_field_name("body")
                .filter(|body| {
                    body.start_byte() <= node.start_byte() && node.end_byte() <= body.end_byte()
                })
                .map(|_| name));
        }
        if matches!(
            current.kind(),
            "type_expression" | "pragma_list" | "generic_argument_list" | "call"
        ) {
            return Ok(None);
        }
        parent = current.parent();
    }
    Ok(None)
}

fn has_binding(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    loop {
        cancellation.check()?;
        if matches!(
            cursor.node().kind(),
            "proc_declaration"
                | "func_declaration"
                | "macro_declaration"
                | "template_declaration"
                | "method_declaration"
                | "iterator_declaration"
                | "converter_declaration"
                | "symbol_declaration"
                | "tuple_deconstruct_declaration"
                | "type_declaration"
                | "include_statement"
                | "mixin_statement"
                | "bind_statement"
        ) {
            return Ok(true);
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(false);
            }
        }
    }
}

fn text<'source>(node: Node<'_>, source: &'source [u8]) -> Option<&'source str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn same_name(left: &str, right: &str) -> bool {
    let mut left = left.chars();
    let mut right = right.chars();
    left.next() == right.next()
        && left
            .filter(|character| *character != '_')
            .map(|character| character.to_ascii_lowercase())
            .eq(right
                .filter(|character| *character != '_')
                .map(|character| character.to_ascii_lowercase()))
}

fn lexical_scope<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut parent = node.parent();
    while let Some(current) = parent {
        cancellation.check()?;
        if matches!(
            current.kind(),
            "source_file"
                | "proc_declaration"
                | "func_declaration"
                | "macro_declaration"
                | "template_declaration"
                | "method_declaration"
                | "iterator_declaration"
                | "converter_declaration"
                | "block"
                | "do_block"
        ) {
            return Ok(Some(current));
        }
        parent = current.parent();
    }
    Ok(None)
}

fn contains(outer: Node<'_>, inner: Node<'_>) -> bool {
    outer.start_byte() <= inner.start_byte() && inner.end_byte() <= outer.end_byte()
}

#[derive(Clone, Copy)]
enum ValueKind {
    Numeric,
    String,
    Character,
    Boolean,
}

fn written_type(node: Node<'_>, source: &[u8]) -> Option<ValueKind> {
    let ty = text(node, source)?;
    let ty = ty.strip_prefix("var ").unwrap_or(ty);
    if matches!(
        ty,
        "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "float"
            | "float32"
            | "float64"
    ) {
        Some(ValueKind::Numeric)
    } else {
        match ty {
            "string" | "cstring" => Some(ValueKind::String),
            "char" => Some(ValueKind::Character),
            "bool" => Some(ValueKind::Boolean),
            _ => None,
        }
    }
}

enum ArgumentEvidence {
    Known(ValueKind),
    Unknown,
    Ambiguous,
}

fn literal_kind(argument: Node<'_>) -> Option<ValueKind> {
    match argument.kind() {
        "interpreted_string_literal" | "raw_string_literal" | "long_string_literal" => {
            Some(ValueKind::String)
        }
        "integer_literal" | "float_literal" => Some(ValueKind::Numeric),
        "char_literal" => Some(ValueKind::Character),
        "boolean_literal" => Some(ValueKind::Boolean),
        _ => None,
    }
}

fn argument_evidence(
    mut argument: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<ArgumentEvidence, AdapterError> {
    loop {
        cancellation.check()?;
        if let Some(kind) = literal_kind(argument) {
            return Ok(ArgumentEvidence::Known(kind));
        }
        let Some(written) = leaf(argument).and_then(|name| text(name, source)) else {
            return Ok(ArgumentEvidence::Unknown);
        };
        let mut root = argument;
        while let Some(parent) = root.parent() {
            cancellation.check()?;
            root = parent;
        }
        let mut cursor = root.walk();
        let mut nearest = None;
        let mut extent = usize::MAX;
        let mut ambiguous = false;
        loop {
            cancellation.check()?;
            let node = cursor.node();
            if node.kind() == "symbol_declaration"
                && node.start_byte() < argument.start_byte()
                && name(node)
                    .and_then(|name| text(name, source))
                    .is_some_and(|name| same_name(name, written))
                && let Some(scope) = lexical_scope(node, cancellation)?
                && contains(scope, argument)
            {
                let candidate_extent = scope.end_byte() - scope.start_byte();
                if candidate_extent < extent {
                    nearest = Some(node);
                    extent = candidate_extent;
                    ambiguous = false;
                } else if candidate_extent == extent {
                    ambiguous = true;
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    break;
                }
            }
            if cursor.node() == root {
                break;
            }
        }
        if ambiguous {
            return Ok(ArgumentEvidence::Ambiguous);
        }
        let Some(binding) = nearest else {
            return Ok(ArgumentEvidence::Unknown);
        };
        if !visible(binding, cancellation)? || inside_block(binding, cancellation)? {
            return Ok(ArgumentEvidence::Ambiguous);
        }
        let Some(declaration) = binding.parent().and_then(|parent| parent.parent()) else {
            return Ok(ArgumentEvidence::Unknown);
        };
        if let Some(kind) = declaration
            .child_by_field_name("type")
            .and_then(|ty| written_type(ty, source))
        {
            return Ok(ArgumentEvidence::Known(kind));
        }
        let Some(initializer) = declaration.child_by_field_name("value") else {
            return Ok(ArgumentEvidence::Unknown);
        };
        // Initializer aliases move strictly backward through source declarations;
        // repeated names or an unavailable context never establish a value type.
        if initializer.start_byte() >= argument.start_byte() {
            return Ok(ArgumentEvidence::Unknown);
        }
        argument = initializer;
    }
}

fn incompatible_argument(
    argument: Node<'_>,
    parameter: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let evidence = argument_evidence(argument, source, cancellation)?;
    if matches!(evidence, ArgumentEvidence::Ambiguous) {
        return Ok(true);
    }
    let Some(expected) = parameter
        .child_by_field_name("type")
        .and_then(|ty| written_type(ty, source))
    else {
        return Ok(false);
    };
    // Source compatibility rejects contradictions, without claiming imported
    // overload selection, conversions or compiler-evaluated type equivalence.
    Ok(matches!(
        (evidence, expected),
        (
            ArgumentEvidence::Known(ValueKind::String),
            ValueKind::Numeric | ValueKind::Character | ValueKind::Boolean
        ) | (
            ArgumentEvidence::Known(ValueKind::Numeric),
            ValueKind::String
        )
    ))
}

fn incompatible_arguments(
    call: Node<'_>,
    target: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut children = call.walk();
    let Some(arguments) = call
        .named_children(&mut children)
        .find(|child| child.kind() == "argument_list")
    else {
        return Ok(false);
    };
    let Some(parameters) = target.child_by_field_name("parameters") else {
        return Ok(false);
    };
    let mut arguments_cursor = arguments.walk();
    let mut arguments = arguments.named_children(&mut arguments_cursor);
    let mut parameters_cursor = parameters.walk();
    for parameter in parameters.named_children(&mut parameters_cursor) {
        cancellation.check()?;
        let Some(names) = parameter
            .named_child(0)
            .filter(|names| names.kind() == "symbol_declaration_list")
        else {
            return Ok(false);
        };
        let mut names_cursor = names.walk();
        for _ in names.named_children(&mut names_cursor) {
            cancellation.check()?;
            let Some(argument) = arguments.next() else {
                return Ok(false);
            };
            if incompatible_argument(argument, parameter, source, cancellation)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// A conditional binding affects its written name, not every unrelated module
// function. Block and quoted bodies cannot leak bindings into an outer scope.
fn direct_binding_available(
    call: Node<'_>,
    called: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if inside_block(call, cancellation)? {
        return Ok(false);
    }
    let Some(called) = text(called, source) else {
        return Ok(false);
    };
    let mut root = call;
    while let Some(parent) = root.parent() {
        cancellation.check()?;
        root = parent;
    }
    let mut cursor = root.walk();
    loop {
        cancellation.check()?;
        let node = cursor.node();
        let scope = lexical_scope(node, cancellation)?;
        if scope.is_some_and(|scope| contains(scope, call)) {
            if matches!(
                node.kind(),
                "include_statement"
                    | "mixin_statement"
                    | "bind_statement"
                    | "tuple_deconstruct_declaration"
            ) {
                return Ok(false);
            }
            if let Some(written) = name(node).and_then(|name| text(name, source))
                && same_name(written, called)
                && matches!(
                    node.kind(),
                    "proc_declaration"
                        | "func_declaration"
                        | "macro_declaration"
                        | "template_declaration"
                        | "method_declaration"
                        | "iterator_declaration"
                        | "converter_declaration"
                        | "symbol_declaration"
                        | "type_declaration"
                )
                && (!visible(node, cancellation)?
                    || !matches!(node.kind(), "proc_declaration" | "func_declaration")
                    || node.child_by_field_name("body").is_none()
                    || incompatible_arguments(call, node, source, cancellation)?)
            {
                return Ok(false);
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(true);
            }
        }
    }
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    Ok(match role {
        StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope => {
            node.kind() == "source_file"
        }
        StructuralRole::Declaration | StructuralRole::Definition => {
            declaration(node, cancellation)?.is_some_and(|(_, name)| {
                source
                    .get(name.byte_range())
                    .and_then(|raw| std::str::from_utf8(raw).ok())
                    .and_then(|text| structural_captured_name_for_language("nim", text, text.len()))
                    .is_some()
            })
        }
        StructuralRole::Signature => node.end_byte() > node.start_byte(),
        StructuralRole::Reference => call_name(node, cancellation)?.is_some(),
        StructuralRole::Expression => true,
        StructuralRole::Comment | StructuralRole::StringLiteral => !node.is_missing(),
        _ => false,
    })
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    let mut range = node.byte_range();
    let syntax = match role {
        StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope => {
            range = 0..source.len();
            "nim.file"
        }
        StructuralRole::Declaration => {
            declaration(node, cancellation)?
                .ok_or_else(|| query_failure("query-nim-owner"))?
                .0
        }
        StructuralRole::Definition => {
            range = declaration(node, cancellation)?
                .ok_or_else(|| query_failure("query-nim-owner"))?
                .1
                .byte_range();
            "nim.name"
        }
        StructuralRole::Signature => {
            if declaration(node, cancellation)?.is_some() {
                if let Some(body) = node.child_by_field_name("body") {
                    range.end = body.start_byte();
                }
                "nim.written_header"
            } else {
                "nim.declaration_unavailable"
            }
        }
        StructuralRole::Reference => {
            let called =
                call_name(node, cancellation)?.ok_or_else(|| query_failure("query-nim-callee"))?;
            range = called.byte_range();
            if direct_binding_available(node, called, source, cancellation)? {
                "nim.call"
            } else {
                "nim.unresolved_call"
            }
        }
        StructuralRole::Expression => {
            if has_binding(node, cancellation)? {
                "nim.binding_unavailable"
            } else {
                "nim.context_unavailable"
            }
        }
        StructuralRole::Comment => "nim.comment",
        StructuralRole::StringLiteral => "nim.string",
        _ => return Err(query_failure("query-nim-role")),
    };
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        syntax,
        role,
        required: role.belongs_to_identity_closure(),
        native_depth: native_depth(node, cancellation)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;
    #[test]
    fn native_context_walks_observe_cancellation() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_nim::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse("proc target() = discard\n", None).unwrap();
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            retain_capture(tree.root_node(), StructuralRole::Root, b"", &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
