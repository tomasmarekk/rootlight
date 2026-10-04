//! F# selectors retain source-backed declaration and import evidence.
//! Repeated recursive heads and signature contracts need native ownership checks.

use std::ops::Range;

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::StructuralRole;

fn valid(node: Node<'_>) -> bool {
    !node.is_missing() && !node.has_error() && node.start_byte() < node.end_byte()
}

fn direct_kind<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    let mut selected = None;
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() && child.kind() == kind && selected.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(selected.filter(|node| valid(*node)))
}

fn has_direct_kind(
    node: Node<'_>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() && child.kind() == kind && valid(child) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn signature_member_kind(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    let Some(spec) = direct_kind(node, "curried_spec", cancellation)? else {
        return Ok("fsharp.signature_member");
    };
    // Indexed property contracts have argument specs too. Only direct written
    // accessor tokens distinguish them from an ordinary curried method.
    let accessor = has_direct_kind(node, "with", cancellation)?
        && (has_direct_kind(node, "get", cancellation)?
            || has_direct_kind(node, "set", cancellation)?);
    // A parenthesized function-valued property has no direct arguments_spec.
    // Inspecting arrow text would wrongly promote its nested return type.
    Ok(
        if !accessor && has_direct_kind(spec, "arguments_spec", cancellation)? {
            "fsharp.signature_method"
        } else {
            "fsharp.signature_property"
        },
    )
}

fn ambiguous_type_abbreviation(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    let mut selected = None;
    for child in node.children_by_field_name("block", &mut cursor) {
        cancellation.check()?;
        if child.is_named() && !child.is_extra() && selected.replace(child).is_some() {
            return Ok(false);
        }
    }
    let Some(mut ty) = selected.filter(|node| valid(*node)) else {
        return Ok(false);
    };
    while ty.kind() == "paren_type" {
        cancellation.check()?;
        let mut cursor = ty.walk();
        let mut selected = None;
        for child in ty.named_children(&mut cursor) {
            cancellation.check()?;
            if !child.is_extra() && selected.replace(child).is_some() {
                return Ok(false);
            }
        }
        let Some(inner) = selected.filter(|node| valid(*node)) else {
            return Ok(false);
        };
        ty = inner;
    }
    if ty.kind() != "simple_type" {
        return Ok(false);
    }
    let Some(path) = direct_kind(ty, "long_identifier", cancellation)? else {
        return Ok(false);
    };
    let mut cursor = path.walk();
    let mut identifiers = 0usize;
    for child in path.named_children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() {
            continue;
        }
        if child.kind() != "identifier" {
            return Ok(false);
        }
        identifiers = identifiers
            .checked_add(1)
            .ok_or_else(|| super::query_failure("fsharp-type-name-count"))?;
    }
    Ok(identifiers == 1)
}

fn object_type_kind(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    if has_direct_kind(node, "struct", cancellation)? {
        return Ok("fsharp.struct");
    }
    if has_direct_kind(node, "class", cancellation)?
        || has_direct_kind(node, "primary_constr_args", cancellation)?
    {
        return Ok("fsharp.class");
    }
    let mut cursor = node.walk();
    for member in node.named_children(&mut cursor) {
        cancellation.check()?;
        if member.kind() == "member_defn"
            && (has_direct_kind(member, "constructor_signature", cancellation)?
                || has_direct_kind(member, "additional_constr_defn", cancellation)?)
        {
            return Ok("fsharp.class");
        }
        if member.kind() == "type_extension_elements" {
            let mut cursor = member.walk();
            for declaration in member.named_children(&mut cursor) {
                cancellation.check()?;
                if declaration.kind() == "member_defn"
                    && (has_direct_kind(declaration, "constructor_signature", cancellation)?
                        || has_direct_kind(declaration, "additional_constr_defn", cancellation)?)
                {
                    return Ok("fsharp.class");
                }
            }
        }
    }
    // The grammar shares this shape between implicit interfaces and classes.
    // Abstract members alone cannot prove either representation.
    Ok("fsharp.ambiguous_type")
}

fn single_name<'tree>(
    mut node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    loop {
        cancellation.check()?;
        if !valid(node) {
            return Ok(None);
        }
        if matches!(node.kind(), "identifier" | "op_identifier") {
            return Ok(Some(node));
        }
        if !matches!(
            node.kind(),
            "identifier_pattern"
                | "long_identifier_or_op"
                | "long_identifier"
                | "property_or_ident"
        ) {
            return Ok(None);
        }
        let mut cursor = node.walk();
        let mut selected = None;
        for child in node.named_children(&mut cursor) {
            cancellation.check()?;
            if !child.is_extra() && selected.replace(child).is_some() {
                return Ok(None);
            }
        }
        let Some(child) = selected else {
            return Ok(None);
        };
        node = child;
    }
}

fn value_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    let mut selected = None;
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() || matches!(child.kind(), "access_modifier" | "type_arguments") {
            continue;
        }
        if selected.replace(child).is_some() {
            return Ok(None);
        }
    }
    match selected {
        Some(pattern) if pattern.kind() == "identifier_pattern" => {
            single_name(pattern, cancellation)
        }
        _ => Ok(None),
    }
}

fn selected_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    match node.kind() {
        "namespace" | "named_module" => {
            let mut cursor = node.walk();
            let mut selected = None;
            for name in node.children_by_field_name("name", &mut cursor) {
                cancellation.check()?;
                if name.kind() == "long_identifier" && selected.replace(name).is_some() {
                    return Ok(None);
                }
            }
            Ok(selected.filter(|name| valid(*name)))
        }
        "module_defn" | "record_field" | "union_type_case" | "enum_type_case" => {
            direct_kind(node, "identifier", cancellation)
        }
        "union_type_field" => {
            if direct_kind(node, ":", cancellation)?.is_some() {
                direct_kind(node, "identifier", cancellation)
            } else {
                Ok(None)
            }
        }
        "function_declaration_left" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                cancellation.check()?;
                if matches!(child.kind(), "identifier" | "op_identifier") {
                    return Ok(valid(child).then_some(child));
                }
                if !child.is_extra() && child.kind() != "access_modifier" {
                    return Ok(None);
                }
            }
            Ok(None)
        }
        "value_declaration_left" => value_name(node, cancellation),
        "value_definition" => match direct_kind(node, "value_declaration_left", cancellation)? {
            Some(left) => value_name(left, cancellation),
            None => Ok(None),
        },
        "record_type_defn"
        | "union_type_defn"
        | "type_abbrev_defn"
        | "enum_type_defn"
        | "anon_type_defn"
        | "interface_type_defn" => Ok(direct_kind(node, "type_name", cancellation)?
            .and_then(|head| head.child_by_field_name("type_name"))
            .filter(|name| valid(*name))),
        "method_or_prop_defn" => {
            let Some(name) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            if let Some(method) = name.child_by_field_name("method") {
                return Ok(valid(method).then_some(method));
            }
            single_name(name, cancellation)
        }
        "member_signature" => node.child_by_field_name("name").map_or_else(
            || direct_kind(node, "identifier", cancellation),
            |name| Ok(valid(name).then_some(name)),
        ),
        "member_defn" => {
            if direct_kind(node, "method_or_prop_defn", cancellation)?.is_some()
                || direct_kind(node, "member_signature", cancellation)?.is_some()
                || direct_kind(node, "additional_constr_defn", cancellation)?.is_some()
                || direct_kind(node, "constructor_signature", cancellation)?.is_some()
                || direct_kind(node, "value_declaration", cancellation)?.is_some()
            {
                return Ok(None);
            }
            if let Some(name) = direct_kind(node, "property_or_ident", cancellation)? {
                return single_name(name, cancellation);
            }
            if direct_kind(node, "val", cancellation)?.is_some() {
                return direct_kind(node, "identifier", cancellation);
            }
            Ok(None)
        }
        "additional_constr_defn" | "constructor_signature" => {
            direct_kind(node, "new", cancellation)
        }
        _ => Ok(None),
    }
}

#[derive(Debug)]
struct BindingBounds {
    declaration: Range<usize>,
    signature: Range<usize>,
}

fn local_expression_binding(
    head: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    let Some(expression) = head
        .parent()
        .filter(|node| node.kind() == "function_or_value_defn")
        .and_then(|group| group.parent())
    else {
        return Ok(false);
    };
    Ok(matches!(
        expression.kind(),
        "declaration_expression" | "comp_declaration_expression"
    ) && expression.child_by_field_name("in").is_some())
}

fn binding_bounds(
    head: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<BindingBounds>, AdapterError> {
    let Some(parent) = head
        .parent()
        .filter(|node| node.kind() == "function_or_value_defn")
    else {
        return Ok(None);
    };
    if !valid(head) || !valid(parent) {
        return Ok(None);
    }
    let mut cursor = parent.walk();
    if !cursor.goto_first_child() {
        return Ok(None);
    }
    let mut active = false;
    let mut equals = None;
    let mut body = None;
    loop {
        cancellation.check()?;
        let child = cursor.node();
        if child == head {
            active = true;
        } else if active && !child.is_extra() {
            if matches!(
                child.kind(),
                "function_declaration_left" | "value_declaration_left" | "and"
            ) {
                break;
            }
            if child.kind() == "=" && equals.replace(child).is_some() {
                return Ok(None);
            }
            if cursor.field_name() == Some("body") && body.replace(child).is_some() {
                return Ok(None);
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    let (Some(equals), Some(body)) = (equals, body) else {
        return Ok(None);
    };
    if !valid(body)
        || head.end_byte() > equals.start_byte()
        || equals.end_byte() > body.start_byte()
    {
        return Ok(None);
    }
    // Repeated body fields belong to the head immediately before them. Using
    // child_by_field_name("body") or the parent range merges recursive siblings.
    Ok(Some(BindingBounds {
        declaration: head.start_byte()..body.end_byte(),
        signature: head.start_byte()..equals.start_byte(),
    }))
}

fn written_kind(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    cancellation.check()?;
    Ok(Some(match node.kind() {
        "namespace" => "fsharp.namespace",
        "named_module" => "fsharp.module",
        "module_defn" => "fsharp.nested_module",
        "function_declaration_left"
            if !local_expression_binding(node, cancellation)?
                && binding_bounds(node, cancellation)?.is_some() =>
        {
            "fsharp.function"
        }
        "value_declaration_left"
            if !local_expression_binding(node, cancellation)?
                && binding_bounds(node, cancellation)?.is_some() =>
        {
            "fsharp.variable"
        }
        "record_type_defn" => "fsharp.record",
        "union_type_defn" => "fsharp.union",
        "type_abbrev_defn" if ambiguous_type_abbreviation(node, cancellation)? => {
            "fsharp.ambiguous_type"
        }
        "type_abbrev_defn" => "fsharp.type_alias",
        "enum_type_defn" => "fsharp.enum",
        "interface_type_defn" => "fsharp.interface",
        "anon_type_defn" => return object_type_kind(node, cancellation).map(Some),
        "record_field"
            if node
                .parent()
                .and_then(|fields| fields.parent())
                .is_some_and(|owner| owner.kind() == "record_type_defn") =>
        {
            "fsharp.record_field"
        }
        "union_type_case" => "fsharp.union_case",
        "union_type_field" => "fsharp.union_field",
        "enum_type_case" => "fsharp.enum_constant",
        "method_or_prop_defn" if node.child_by_field_name("args").is_some() => "fsharp.method",
        "method_or_prop_defn" => "fsharp.property",
        "additional_constr_defn" => "fsharp.constructor",
        "value_definition" => "fsharp.signature_val",
        "member_signature"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "member_defn") =>
        {
            return signature_member_kind(node, cancellation).map(Some);
        }
        "constructor_signature" => "fsharp.signature_new",
        "member_defn" if direct_kind(node, "property_or_ident", cancellation)?.is_some() => {
            "fsharp.property"
        }
        "member_defn" if direct_kind(node, "val", cancellation)?.is_some() => {
            "fsharp.signature_field"
        }
        _ => return Ok(None),
    }))
}

fn unsupported_declaration(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    cancellation.check()?;
    Ok(match node.kind() {
        "module_abbrev" => Some("fsharp.unsupported_module_alias"),
        "exception_definition" => Some("fsharp.unsupported_exception"),
        "type_extension" => Some("fsharp.unsupported_type_extension"),
        "delegate_type_defn" => Some("fsharp.unsupported_delegate"),
        "type_declaration" => Some("fsharp.unsupported_abstract_type"),
        "argument_patterns" | "argument_name_spec" => Some("fsharp.unsupported_parameters"),
        "union_type_field" if selected_name(node, cancellation)?.is_none() => {
            Some("fsharp.unsupported_union_field")
        }
        "object_expression" => Some("fsharp.unsupported_anonymous_object"),
        "function_declaration_left" | "value_declaration_left" | "value_definition"
            if written_kind(node, cancellation)?.is_none()
                || selected_name(node, cancellation)?.is_none() =>
        {
            Some("fsharp.unsupported_binding")
        }
        "record_field" if written_kind(node, cancellation)?.is_none() => {
            Some("fsharp.unsupported_anonymous_record_field")
        }
        _ => None,
    })
}

/// Retains only captures with reviewed source names and unambiguous ownership.
///
/// Cancellation is propagated. Unsupported declarations retain gap markers, and
/// executable relationship roles are never admitted by this structural adapter.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if matches!(role, StructuralRole::Root | StructuralRole::Module) {
        return Ok(node.kind() == "file");
    }
    if role == StructuralRole::Scope
        && matches!(
            node.kind(),
            "file" | "namespace" | "named_module" | "module_defn"
        )
    {
        return Ok(!node.is_missing());
    }
    if !valid(node) {
        return Ok(false);
    }
    if role == StructuralRole::Declaration && unsupported_declaration(node, cancellation)?.is_some()
    {
        return Ok(true);
    }
    if role == StructuralRole::Import {
        return Ok(node.kind() == "import_decl"
            && direct_kind(node, "long_identifier", cancellation)?.is_some());
    }
    if matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Scope
    ) {
        return Ok(written_kind(node, cancellation)?.is_some()
            && selected_name(node, cancellation)?.is_some());
    }
    Ok(false)
}

/// Returns the syntax label before the common role suffix is attached.
///
/// All labels describe written syntax; cancellation is propagated and unknown
/// shapes return None. Import labels record `open type` without resolving it.
pub(super) fn syntax(
    node: Node<'_>,
    role: StructuralRole,
    _source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<&'static str>, AdapterError> {
    cancellation.check()?;
    if node.kind() == "file" {
        return Ok(Some("fsharp.file"));
    }
    if role == StructuralRole::Declaration
        && let Some(unsupported) = unsupported_declaration(node, cancellation)?
    {
        return Ok(Some(unsupported));
    }
    if role == StructuralRole::Import {
        return Ok(Some(
            if direct_kind(node, "type", cancellation)?.is_some() {
                "fsharp.open_type"
            } else {
                "fsharp.open"
            },
        ));
    }
    if role == StructuralRole::Definition {
        return Ok(
            selected_name(node, cancellation)?.map(|name| match name.kind() {
                "op_identifier" => "fsharp.operator_name",
                "new" => "fsharp.new_name",
                _ => "fsharp.name",
            }),
        );
    }
    written_kind(node, cancellation)
}

/// Returns exact role-specific byte bounds, never an entire recursive group.
///
/// Unsupported head/body pairs return None. Declaration and scope spans for a
/// recursive head cover only that head and its own body; signatures stop at `=`.
pub(super) fn capture_range(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    cancellation.check()?;
    if role == StructuralRole::Definition {
        return Ok(selected_name(node, cancellation)?.map(|name| name.byte_range()));
    }
    if role == StructuralRole::Declaration && unsupported_declaration(node, cancellation)?.is_some()
    {
        if matches!(
            node.kind(),
            "function_declaration_left" | "value_declaration_left"
        ) && let Some(bounds) = binding_bounds(node, cancellation)?
        {
            return Ok(Some(bounds.declaration));
        }
        return Ok(Some(node.byte_range()));
    }
    if matches!(
        node.kind(),
        "function_declaration_left" | "value_declaration_left"
    ) {
        return Ok(binding_bounds(node, cancellation)?.map(|bounds| {
            if role == StructuralRole::Signature {
                bounds.signature
            } else {
                bounds.declaration
            }
        }));
    }
    if role != StructuralRole::Signature {
        return Ok(Some(node.byte_range()));
    }
    if matches!(
        node.kind(),
        "type_abbrev_defn" | "member_signature" | "constructor_signature"
    ) {
        return Ok(Some(node.byte_range()));
    }
    if matches!(node.kind(), "namespace" | "named_module") {
        return Ok(
            selected_name(node, cancellation)?.map(|name| node.start_byte()..name.end_byte())
        );
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() && matches!(child.kind(), "=" | "with") {
            return Ok((node.start_byte() < child.start_byte())
                .then_some(node.start_byte()..child.start_byte()));
        }
    }
    Ok(Some(node.byte_range()))
}

/// Identifies same-node lexical environments that need depth below a declaration.
///
/// Query integration must retain native depth for F# and add one only for these
/// scope captures, so declarations are not owned by their own same-span scope.
pub(super) fn scope_below_declaration(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "namespace"
            | "named_module"
            | "module_defn"
            | "function_declaration_left"
            | "value_declaration_left"
            | "record_type_defn"
            | "union_type_defn"
            | "enum_type_defn"
            | "anon_type_defn"
            | "interface_type_defn"
            | "method_or_prop_defn"
            | "additional_constr_defn"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str, signature: bool) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let language = if signature {
            tree_sitter_fsharp::LANGUAGE_SIGNATURE
        } else {
            tree_sitter_fsharp::LANGUAGE_FSHARP
        };
        parser.set_language(&language.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        tree
    }

    fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
        let mut output = Vec::new();
        let mut cursor = root.walk();
        loop {
            if cursor.node().kind() == kind {
                output.push(cursor.node());
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return output;
                }
            }
        }
    }

    #[test]
    fn recursive_heads_keep_disjoint_body_bounds() {
        let source = "module Rec\nlet rec first n = n + 1\nand second n = n + 2\n";
        let tree = parse(source, false);
        let cancellation = Cancellation::new();
        let heads = nodes(tree.root_node(), "function_declaration_left");
        assert_eq!(heads.len(), 2);
        let ranges: Vec<_> = heads
            .iter()
            .map(|head| {
                binding_bounds(*head, &cancellation)
                    .unwrap()
                    .unwrap()
                    .declaration
            })
            .collect();
        assert_eq!(source.get(ranges[0].clone()), Some("first n = n + 1"));
        assert_eq!(source.get(ranges[1].clone()), Some("second n = n + 2"));
        assert!(ranges[0].end <= ranges[1].start);
    }

    #[test]
    fn signature_contracts_keep_written_kind_labels() {
        let source = include_str!("../../tests/fixtures/fsharp/structural.fsi");
        let tree = parse(source, true);
        let cancellation = Cancellation::new();
        for (native, expected) in [
            ("value_definition", "fsharp.signature_val"),
            ("constructor_signature", "fsharp.signature_new"),
        ] {
            let captured = nodes(tree.root_node(), native);
            assert!(!captured.is_empty(), "{native}");
            for node in captured {
                assert_eq!(written_kind(node, &cancellation).unwrap(), Some(expected));
            }
        }
        let members = nodes(tree.root_node(), "member_signature");
        assert_eq!(members.len(), 2);
        assert_eq!(
            written_kind(members[0], &cancellation).unwrap(),
            Some("fsharp.signature_method")
        );
        assert_eq!(
            written_kind(members[1], &cancellation).unwrap(),
            Some("fsharp.signature_property")
        );
    }

    #[test]
    fn function_valued_property_does_not_acquire_a_method_label() {
        let source = "module Contracts\ntype I =\n    abstract member FunctionValue: (int -> int)\n    abstract member Curried: int -> int\n";
        let tree = parse(source, true);
        let cancellation = Cancellation::new();
        let members = nodes(tree.root_node(), "member_signature");
        assert_eq!(members.len(), 2);
        assert_eq!(
            written_kind(members[0], &cancellation).unwrap(),
            Some("fsharp.signature_property")
        );
        assert_eq!(
            written_kind(members[1], &cancellation).unwrap(),
            Some("fsharp.signature_method")
        );
    }

    #[test]
    fn unqualified_type_abbreviations_keep_representation_ambiguity() {
        let tree = parse(
            "module Types\ntype Ambiguous = string\ntype Qualified = System.String\n",
            false,
        );
        let cancellation = Cancellation::new();
        let types = nodes(tree.root_node(), "type_abbrev_defn");
        assert_eq!(types.len(), 2);
        assert_eq!(
            written_kind(types[0], &cancellation).unwrap(),
            Some("fsharp.ambiguous_type")
        );
        assert_eq!(
            written_kind(types[1], &cancellation).unwrap(),
            Some("fsharp.type_alias")
        );
    }

    #[test]
    fn parentheses_preserve_single_name_type_ambiguity_in_both_profiles() {
        let source = "module Types\ntype Parenthesized = (string)\ntype Nested = ((``QuotedCase``))\ntype Qualified = (System.String)\ntype Generic = (list<int>)\ntype 'a Identity = ('a)\n";
        for signature in [false, true] {
            let tree = parse(source, signature);
            let cancellation = Cancellation::new();
            let types = nodes(tree.root_node(), "type_abbrev_defn");
            assert_eq!(types.len(), 5);
            for (node, expected) in types.into_iter().zip([
                "fsharp.ambiguous_type",
                "fsharp.ambiguous_type",
                "fsharp.type_alias",
                "fsharp.type_alias",
                "fsharp.type_alias",
            ]) {
                assert_eq!(written_kind(node, &cancellation).unwrap(), Some(expected));
            }
        }
    }

    #[test]
    fn abstract_object_contracts_do_not_prove_class_representation() {
        for signature in [false, true] {
            let tree = parse(
                "module Contracts\ntype Unknown =\n    abstract member Read: int -> int\n",
                signature,
            );
            let cancellation = Cancellation::new();
            let types = nodes(tree.root_node(), "anon_type_defn");
            assert_eq!(types.len(), 1);
            assert_eq!(
                written_kind(types[0], &cancellation).unwrap(),
                Some("fsharp.ambiguous_type")
            );
        }
    }

    #[test]
    fn direct_signature_constructor_wrapper_proves_class_representation() {
        let tree = parse(
            include_str!("../../tests/fixtures/fsharp/structural.fsi"),
            true,
        );
        let cancellation = Cancellation::new();
        let types = nodes(tree.root_node(), "anon_type_defn");
        assert_eq!(types.len(), 1);
        assert_eq!(
            written_kind(types[0], &cancellation).unwrap(),
            Some("fsharp.class")
        );
    }

    #[test]
    fn query_packs_compile_only_against_their_matching_factory() {
        for (language, source) in [
            (
                tree_sitter_fsharp::LANGUAGE_FSHARP,
                include_str!("../../queries/fsharp.scm"),
            ),
            (
                tree_sitter_fsharp::LANGUAGE_SIGNATURE,
                include_str!("../../queries/fsharp_signature.scm"),
            ),
        ] {
            let query = tree_sitter::Query::new(&language.into(), source).unwrap();
            let mut captures = query.capture_names().to_vec();
            captures.sort_unstable();
            assert_eq!(
                captures,
                [
                    "declaration",
                    "definition",
                    "import",
                    "module",
                    "root",
                    "scope",
                    "signature"
                ]
            );
        }
    }

    #[test]
    fn destructuring_is_not_a_single_value_declaration() {
        let tree = parse("module Patterns\nlet (left, right) = (1, 2)\n", false);
        let cancellation = Cancellation::new();
        let heads = nodes(tree.root_node(), "value_declaration_left");
        assert_eq!(heads.len(), 1);
        assert!(retain_capture(heads[0], StructuralRole::Declaration, &cancellation).unwrap());
        assert_eq!(
            syntax(heads[0], StructuralRole::Declaration, b"", &cancellation).unwrap(),
            Some("fsharp.unsupported_binding")
        );
        assert!(!retain_capture(heads[0], StructuralRole::Definition, &cancellation).unwrap());
    }
}
