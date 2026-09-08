//! Dart declaration positions and exact callable name ranges.
//! Constructor names may span multiple native name fields; calls are not bindings.

use std::ops::Range;

use tree_sitter::Node;

use super::StructuralRole;

pub(super) fn retain_capture(node: Node<'_>, role: StructuralRole) -> bool {
    if node.kind() != "identifier" {
        return true;
    }
    match role {
        StructuralRole::Declaration => binding_syntax(node).is_some(),
        StructuralRole::Definition => {
            binding_syntax(node).is_some()
                || node.parent().is_some_and(|parent| {
                    (matches!(
                        parent.kind(),
                        "class_declaration"
                            | "mixin_declaration"
                            | "extension_declaration"
                            | "extension_type_declaration"
                            | "enum_declaration"
                            | "enum_constant"
                            | "extension_type_representation"
                    ) && parent.child_by_field_name("name") == Some(node))
                        || (matches!(
                            parent.kind(),
                            "extension_type_name" | "mixin_application_class"
                        ) && parent.named_child(0) == Some(node))
                })
        }
        _ => true,
    }
}

fn binding_syntax(node: Node<'_>) -> Option<&'static str> {
    let parent = node.parent()?;
    match parent.kind() {
        "formal_parameter" | "constructor_param" | "super_formal_parameter" => {
            Some("dart.parameter")
        }
        "initialized_identifier" | "static_final_declaration" => {
            if parent.child_by_field_name("name") != Some(node) {
                return None;
            }
            let owner = parent.parent()?.parent()?;
            Some(if owner.kind() == "declaration" {
                "dart.field"
            } else {
                "dart.variable"
            })
        }
        "identifier_list" => Some(if parent.parent()?.kind() == "declaration" {
            "dart.field"
        } else {
            "dart.variable"
        }),
        "initialized_variable_definition" | "variable_pattern" | "for_statement"
            if parent.child_by_field_name("name") == Some(node) =>
        {
            Some("dart.variable")
        }
        "catch_clause" => Some("dart.parameter"),
        "constant_pattern" if parent.named_child_count() == 1 => {
            let mut child = parent;
            while let Some(owner) = child.parent() {
                match owner.kind() {
                    "pattern_variable_declaration" if owner.named_child(0) == Some(child) => {
                        return Some("dart.variable");
                    }
                    "list_pattern" | "record_pattern" | "object_pattern" | "map_pattern"
                    | "rest_pattern" => child = owner,
                    _ => return None,
                }
            }
            None
        }
        _ => None,
    }
}

fn callable(node: Node<'_>) -> Node<'_> {
    if node.kind() == "method_signature" {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| child.kind().ends_with("_signature"))
            .unwrap_or(node)
    } else {
        node
    }
}

pub(super) fn definition_range(node: Node<'_>) -> Range<usize> {
    let node = callable(node);
    if node.kind().ends_with("_signature") {
        let mut cursor = node.walk();
        let mut names = node.children_by_field_name("name", &mut cursor);
        if let Some(first) = names.next() {
            let end = names.last().unwrap_or(first).end_byte();
            return first.start_byte()..end;
        }
        if let Some(operator) = node.child_by_field_name("operator") {
            return operator.byte_range();
        }
    }
    node.byte_range()
}

pub(super) fn declaration_syntax(node: Node<'_>) -> Option<&'static str> {
    if node.kind() == "identifier" {
        return binding_syntax(node);
    }
    if matches!(node.kind(), "method_declaration" | "declaration") {
        let mut cursor = node.walk();
        let signature = callable(
            node.named_children(&mut cursor)
                .find(|child| child.kind().ends_with("_signature"))?,
        );
        return Some(if signature.kind().contains("constructor") {
            "dart.constructor"
        } else {
            "dart.method"
        });
    }
    canonical_syntax(node.kind())
}

pub(super) fn header_end(node: Node<'_>) -> usize {
    let mut end = node.end_byte();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if node.child_by_field_name("body") == Some(child)
            || matches!(
                child.kind(),
                "function_body" | "initializers" | "redirection"
            )
        {
            end = end.min(child.start_byte());
        }
        if child.kind() == "method_signature" {
            let mut nested_cursor = child.walk();
            if let Some(initializers) = child
                .named_children(&mut nested_cursor)
                .find(|nested| nested.kind() == "initializers")
            {
                end = end.min(initializers.start_byte());
            }
        }
    }
    end
}

pub(super) fn canonical_syntax(native: &str) -> Option<&'static str> {
    Some(match native {
        "source_file" => "dart.file",
        "class_declaration" | "mixin_application_class" | "extension_type_declaration" => {
            "dart.class"
        }
        "mixin_declaration" => "dart.trait",
        "extension_declaration" => "dart.extension",
        "extension_type_representation" => "dart.field",
        "enum_declaration" => "dart.enum",
        "enum_constant" => "dart.constant",
        "type_alias" => "dart.type",
        "type_parameter" => "dart.parameter",
        "function_declaration"
        | "getter_declaration"
        | "setter_declaration"
        | "external_function_declaration"
        | "external_getter_declaration"
        | "external_setter_declaration"
        | "local_function_declaration" => "dart.function",
        "method_declaration" | "declaration" | "method_signature" => "dart.method",
        "function_signature"
        | "getter_signature"
        | "setter_signature"
        | "operator_signature"
        | "constructor_signature"
        | "constant_constructor_signature"
        | "factory_constructor_signature"
        | "redirecting_factory_constructor_signature" => "dart.callable_name",
        "identifier" => "dart.identifier",
        "type_identifier" => "dart.type_name",
        "import_or_export" => "dart.import",
        "block" => "dart.block",
        "for_statement" => "dart.for",
        "switch_statement_case" | "switch_expression_case" => "dart.case",
        "catch_clause" => "dart.catch",
        "function_expression" => "dart.lambda",
        "comment" => "dart.comment",
        "string_literal" => "dart.string",
        _ => return None,
    })
}
