//! Objective-C source owners and multipart selector capture boundaries.
//! Native label and colon ranges remain separate facts within one written name.

use super::StructuralRole;
use std::ops::Range;
use tree_sitter::Node;

fn primary_name(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == "identifier")
}

fn is_category(node: Node<'_>) -> bool {
    node.kind() == "class_interface" && {
        let mut cursor = node.walk();
        node.children(&mut cursor).any(|child| child.kind() == "(")
    }
}

fn is_ivar_declarator(node: Node<'_>) -> bool {
    node.kind() == "struct_declarator"
        && node
            .parent()
            .and_then(|parent| parent.parent())
            .is_some_and(|parent| parent.kind() == "instance_variable")
}

fn declarator_name(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        match node.kind() {
            "identifier" | "field_identifier" => return Some(node),
            "struct_declarator" => {
                // An unnamed bit-field's width can itself be an identifier;
                // it is an expression after the colon, never a field binding.
                let mut cursor = node.walk();
                node = node.children(&mut cursor).find(|child| !child.is_extra())?;
            }
            "parenthesized_declarator" => {
                let mut cursor = node.walk();
                let mut declarators = node.named_children(&mut cursor).filter(|child| {
                    matches!(
                        child.kind(),
                        "identifier"
                            | "field_identifier"
                            | "array_declarator"
                            | "block_pointer_declarator"
                            | "function_declarator"
                            | "parenthesized_declarator"
                            | "pointer_declarator"
                    )
                });
                let inner = declarators.next()?;
                if declarators.next().is_some() {
                    return None;
                }
                node = inner;
            }
            "array_declarator"
            | "block_pointer_declarator"
            | "function_declarator"
            | "pointer_declarator"
            | "init_declarator" => {
                node = node.child_by_field_name("declarator")?;
            }
            _ => return None,
        }
    }
}

pub(super) fn retain_capture(node: Node<'_>, role: StructuralRole) -> bool {
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition
    ) && is_ivar_declarator(node)
    {
        let mut cursor = node.walk();
        if node
            .children(&mut cursor)
            .find(|child| !child.is_extra())
            .is_some_and(|child| child.kind() == ":")
        {
            // Anonymous bit-fields occupy storage but define no named entity.
            // Retain their expression references without inventing a binding.
            return false;
        }
    }
    match role {
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature => {
            !is_category(node)
        }
        StructuralRole::ScopeType => node.kind() == "class_implementation" || is_category(node),
        StructuralRole::ScopeTrait => node.parent().is_some_and(|parent| {
            (parent.kind() == "class_implementation" || is_category(parent))
                && primary_name(parent) == Some(node)
        }),
        StructuralRole::DefinitionPart => {
            if node.kind() == ":" {
                node.parent()
                    .and_then(|parent| parent.parent())
                    .is_some_and(|owner| {
                        matches!(owner.kind(), "method_declaration" | "method_definition")
                    })
            } else {
                node.kind() == "identifier"
                    && node.parent().is_some_and(|owner| {
                        matches!(owner.kind(), "method_declaration" | "method_definition")
                    })
            }
        }
        _ => true,
    }
}

pub(super) fn capture_syntax(node: Node<'_>, role: StructuralRole) -> Option<&'static str> {
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition
    ) {
        if node
            .parent()
            .is_some_and(|parent| parent.kind() == "method_parameter")
        {
            return Some("objective_c.parameter");
        }
        if is_ivar_declarator(node) || node.kind() == "atomic_declaration" {
            return Some("objective_c.field");
        }
    }
    match role {
        StructuralRole::ScopeTrait => return Some("objective_c.owner"),
        StructuralRole::ScopeType => return Some("objective_c.owner_header"),
        StructuralRole::DefinitionPart => return Some("objective_c.selector"),
        StructuralRole::Scope if is_category(node) => return Some("objective_c.category"),
        StructuralRole::Call => return Some("objective_c.call"),
        _ => {}
    }
    Some(match node.kind() {
        "translation_unit" => "objective_c.file",
        "class_interface" => "objective_c.class_interface",
        "class_implementation" => "objective_c.implementation",
        "protocol_declaration" => "objective_c.protocol",
        "method_declaration" | "method_definition" => "objective_c.method",
        "property_declaration" => "objective_c.property",
        "struct_declarator" => "objective_c.property_name",
        "module_import" => "objective_c.import",
        // Inherited C syntax still belongs to the Objective-C source unit.
        // Keeping its language prefix prevents fenced examples becoming mixed C/ObjC IR.
        "preproc_include" => "objective_c.include",
        "function_definition" => "objective_c.function",
        "declaration" => "objective_c.declaration",
        "struct_specifier" => "objective_c.struct",
        "union_specifier" => "objective_c.union",
        "enum_specifier" => "objective_c.enum",
        "type_definition" => "objective_c.type",
        "compound_statement" => "objective_c.block",
        "identifier" => "objective_c.identifier",
        "field_identifier" => "objective_c.field_identifier",
        "type_identifier" => "objective_c.type_identifier",
        "comment" => "objective_c.comment",
        "string_literal" => "objective_c.string",
        "concatenated_string" => "objective_c.concatenated_string",
        _ => return None,
    })
}

pub(super) fn capture_range(node: Node<'_>, role: StructuralRole) -> Option<Range<usize>> {
    if role == StructuralRole::Definition {
        if node.kind() == "struct_declarator"
            || node
                .parent()
                .is_some_and(|parent| parent.kind() == "method_parameter")
        {
            return declarator_name(node).map(|name| name.byte_range());
        }
        if matches!(node.kind(), "class_interface" | "protocol_declaration") {
            return primary_name(node).map(|name| name.byte_range());
        }
        if matches!(node.kind(), "method_declaration" | "method_definition") {
            let mut start = None;
            let mut end = None;
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "identifier" {
                    start.get_or_insert(child.start_byte());
                    end = Some(child.end_byte());
                } else if child.kind() == "method_parameter"
                    && let Some(colon) = child.child(0).filter(|child| child.kind() == ":")
                {
                    start.get_or_insert(colon.start_byte());
                    end = Some(colon.end_byte());
                }
            }
            return Some(start?..end?);
        }
    }
    if matches!(role, StructuralRole::Signature | StructuralRole::ScopeType) {
        let mut cursor = node.walk();
        let end = node
            .children(&mut cursor)
            .find(|child| {
                node.child_by_field_name("body") == Some(*child)
                    || matches!(
                        child.kind(),
                        "compound_statement"
                            | "instance_variables"
                            | "method_declaration"
                            | "method_definition"
                            | "property_declaration"
                            | "implementation_definition"
                            | "@end"
                            | ";"
                    )
            })
            .map_or(node.end_byte(), |child| child.start_byte());
        return Some(node.start_byte()..end);
    }
    Some(node.byte_range())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GrammarFamily;

    #[test]
    fn written_members_have_clean_native_syntax_and_only_named_definitions() {
        let source = include_bytes!("../../../../tests/fixtures/objective-c/members.m");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_objc::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let pack = super::super::QueryPack::compile(
            GrammarFamily::ObjectiveC,
            include_str!("../../queries/objective_c.scm"),
        )
        .unwrap();
        let candidates = pack
            .extract_identity(
                GrammarFamily::ObjectiveC,
                &tree,
                source,
                1024,
                &rootlight_cancel::Cancellation::new(),
            )
            .unwrap();
        let definitions: Vec<_> = candidates
            .iter()
            .filter(|capture| capture.role == StructuralRole::Definition)
            .map(|capture| source.get(capture.start..capture.end).unwrap())
            .collect();
        assert_eq!(
            definitions.iter().filter(|name| **name == b"first").count(),
            2
        );
        for name in [b"PaddingWidth".as_slice(), b"argument", b"blockArgument"] {
            assert!(!definitions.contains(&name), "{definitions:?}");
        }
    }

    #[test]
    fn selector_headers_retain_class_and_instance_dispatch() {
        let source = b"@interface Sample\n- (int)value;\n+ (int)value;\n@end\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_objc::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let pack = super::super::QueryPack::compile(
            GrammarFamily::ObjectiveC,
            include_str!("../../queries/objective_c.scm"),
        )
        .unwrap();
        let candidates = pack
            .extract_identity(
                GrammarFamily::ObjectiveC,
                &tree,
                source,
                1024,
                &rootlight_cancel::Cancellation::new(),
            )
            .unwrap();
        let headers: Vec<_> = candidates
            .iter()
            .filter(|capture| {
                capture.role == StructuralRole::Signature && capture.syntax == "objective_c.method"
            })
            .map(|capture| source.get(capture.start..capture.end).unwrap())
            .collect();
        assert_eq!(
            headers,
            [b"- (int)value".as_slice(), b"+ (int)value".as_slice()],
            "{candidates:?}"
        );
    }

    #[test]
    fn native_query_matches_the_reviewed_node_schema() {
        tree_sitter::Query::new(
            &tree_sitter_objc::LANGUAGE.into(),
            include_str!("../../queries/objective_c.scm"),
        )
        .expect("Objective-C query matches the pinned native grammar");
    }
}
