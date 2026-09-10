//! Objective-C source owners and multipart selector capture boundaries.
//! Native label and colon ranges remain separate facts within one written name.

use super::StructuralRole;
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use std::{collections::HashMap, ops::Range};
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

fn next_written_sibling(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        node = node.next_sibling()?;
        if !node.is_extra() {
            return Some(node);
        }
    }
}

fn type_parameter_list(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() != "type_identifier" {
        return None;
    }
    let parent = node.parent()?;
    let list = if parent.kind() == "type_name" {
        let mut cursor = parent.walk();
        if parent
            .named_children(&mut cursor)
            .filter(|child| !child.is_extra())
            .take(2)
            .count()
            != 1
        {
            return None;
        }
        // A bound is a use of another type, never another parameter binding.
        let mut previous = parent.prev_sibling();
        while previous.is_some_and(|child| child.is_extra()) {
            previous = previous.and_then(|child| child.prev_sibling());
        }
        if previous.is_some_and(|child| child.kind() == ":") {
            return None;
        }
        parent.parent()?
    } else {
        parent
    };
    (list.kind() == "parameterized_arguments").then_some(list)
}

fn declares_type_parameters(
    list: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(owner) = list.parent() else {
        return Ok(false);
    };
    if owner.kind() == "class_declaration" {
        return Ok(true);
    }
    if owner.kind() != "class_interface" {
        return Ok(false);
    }
    let Some(name) = primary_name(owner) else {
        return Ok(false);
    };
    if next_written_sibling(name) != Some(list) {
        return Ok(false);
    }
    // Before a superclass/category delimiter this is a binding list. A bare
    // root-class list can instead name protocols; variance/bounds disambiguate it.
    if next_written_sibling(list).is_some_and(|next| matches!(next.kind(), ":" | "(")) {
        return Ok(true);
    }
    let mut cursor = list.walk();
    for (index, child) in list.children(&mut cursor).enumerate() {
        if index % 64 == 0 {
            cancellation.check()?;
        }
        if matches!(child.kind(), "__covariant" | "__contravariant" | ":") {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Per-scan memoization prevents repeated whole-list scans for each parameter.
#[derive(Default)]
pub(super) struct TypeParameterCaptures {
    lists: HashMap<usize, bool>,
}

impl TypeParameterCaptures {
    /// Filters parameter candidates using bounded, cancellable list classification.
    ///
    /// # Errors
    /// Returns cancellation or allocation failures from the current query scan.
    pub(super) fn retain(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition
        ) || node.kind() != "type_identifier"
            || !node.parent().is_some_and(|parent| {
                matches!(parent.kind(), "type_name" | "parameterized_arguments")
            })
        {
            return Ok(true);
        }
        let Some(list) = type_parameter_list(node) else {
            return Ok(false);
        };
        if let Some(retained) = self.lists.get(&list.id()) {
            return Ok(*retained);
        }
        let retained = declares_type_parameters(list, cancellation)?;
        self.lists
            .try_reserve(1)
            .map_err(|_| super::query_failure("query-objective-c-parameter-allocation"))?;
        self.lists.insert(list.id(), retained);
        Ok(retained)
    }
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
        if type_parameter_list(node).is_some() {
            return Some("objective_c.type_parameter");
        }
        if node.kind() == "identifier"
            && let Some(parent) = node.parent()
        {
            match parent.kind() {
                "class_declaration" => return Some("objective_c.forward_class"),
                "protocol_forward_declaration" => return Some("objective_c.forward_protocol"),
                _ => {}
            }
        }
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
    if role == StructuralRole::Declaration
        && node.kind() == "identifier"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "class_declaration")
        && let Some(arguments) =
            next_written_sibling(node).filter(|next| next.kind() == "parameterized_arguments")
    {
        // Enclose only this forward's parameters, not those of sibling classes
        // in the same statement. The separate definition capture keeps its name.
        return Some(node.start_byte()..arguments.end_byte());
    }
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
    fn generic_list_classification_is_cached_and_cancellable() {
        let source = b"@interface Bag<__covariant Item, Other>\n@end\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_objc::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let owner = tree.root_node().named_child(0).unwrap();
        let mut cursor = owner.walk();
        let list = owner
            .named_children(&mut cursor)
            .find(|node| node.kind() == "parameterized_arguments")
            .unwrap();
        let mut cursor = list.walk();
        let binding = list
            .named_children(&mut cursor)
            .find(|node| node.kind() == "type_identifier")
            .unwrap();
        let cancellation = Cancellation::new();
        let mut cache = TypeParameterCaptures::default();
        for _ in 0..128 {
            assert!(
                cache
                    .retain(binding, StructuralRole::Declaration, &cancellation)
                    .unwrap()
            );
            assert!(
                cache
                    .retain(binding, StructuralRole::Definition, &cancellation)
                    .unwrap()
            );
        }
        assert_eq!(cache.lists.len(), 1);
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(declares_type_parameters(list, &cancellation).is_err());
    }

    #[test]
    fn generic_bindings_exclude_protocols_bounds_and_superclass_arguments() {
        let source = include_bytes!("../../../../tests/fixtures/objective-c/generics.m");
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
        let names: Vec<_> = candidates
            .iter()
            .filter(|capture| {
                capture.role == StructuralRole::Definition
                    && capture.syntax == "objective_c.type_parameter"
            })
            .map(|capture| {
                std::str::from_utf8(source.get(capture.start..capture.end).unwrap()).unwrap()
            })
            .collect();
        assert_eq!(
            names,
            [
                "Element", "Key", "Value", "Element", "Input", "Other", "Item"
            ]
        );
    }

    #[test]
    fn forward_declarations_and_generic_parameters_have_clean_native_syntax() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_objc::LANGUAGE.into())
            .unwrap();
        let source = include_bytes!("../../../../tests/fixtures/objective-c/forwards.m");
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
    }

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
