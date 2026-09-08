//! R source declaration selectors shared with native capture regression tests.
//! Simple written assignments are not proof of runtime environment or dispatch resolution.

use tree_sitter::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Declaration<'tree> {
    pub(super) name: Node<'tree>,
    pub(super) syntax: &'static str,
    pub(super) function: Option<Node<'tree>>,
}

pub(super) fn declaration<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Declaration<'tree>> {
    if node.kind() == "parameter" {
        return Some(Declaration {
            name: node.child_by_field_name("name")?,
            syntax: "r.parameter",
            function: None,
        });
    }
    if node.parent().is_some_and(|parent| {
        parent.kind() == "for_statement" && parent.child_by_field_name("variable") == Some(node)
    }) {
        return Some(Declaration {
            name: node,
            syntax: "r.variable",
            function: None,
        });
    }
    if node.kind() != "binary_operator" {
        return None;
    }
    let operator = node
        .child_by_field_name("operator")?
        .utf8_text(source)
        .ok()?;
    let (target, value, nonlocal) = match operator {
        "<-" | "=" => ("lhs", "rhs", false),
        "->" => ("rhs", "lhs", false),
        "<<-" => ("lhs", "rhs", true),
        "->>" => ("rhs", "lhs", true),
        _ => return None,
    };
    let name = node.child_by_field_name(target)?;
    // A replacement call, subset or member target writes an existing object;
    // treating its first identifier as a new declaration invents ownership.
    if !matches!(name.kind(), "identifier" | "string" | "dots" | "dot_dot_i") {
        return None;
    }
    let mut value = node.child_by_field_name(value)?;
    while value.kind() == "parenthesized_expression" {
        value = value.child_by_field_name("body")?;
    }
    let function = (value.kind() == "function_definition").then_some(value);
    let syntax = match (function.is_some(), nonlocal) {
        (true, false) => "r.function",
        (false, false) => "r.variable",
        (true, true) => "r.nonlocal_function",
        (false, true) => "r.nonlocal_variable",
    };
    Some(Declaration {
        name,
        syntax,
        function,
    })
}

pub(super) fn signature_range(node: Node<'_>, source: &[u8]) -> Option<std::ops::Range<usize>> {
    let function = declaration(node, source)?.function?;
    let parameters = function.child_by_field_name("parameters")?;
    // Right-assignment names follow the body, so the contiguous callable header
    // begins at `function`/`\` for both assignment directions.
    Some(function.start_byte()..parameters.end_byte())
}

pub(super) fn is_nonlexical_name(node: Node<'_>, source: &[u8]) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if declaration(parent, source).is_some_and(|binding| binding.name == node)
        || declaration(node, source).is_some()
    {
        return true;
    }
    match parent.kind() {
        "namespace_operator" => true,
        "extract_operator" => parent.child_by_field_name("rhs") == Some(node),
        "argument" => parent.child_by_field_name("name") == Some(node),
        _ => false,
    }
}
