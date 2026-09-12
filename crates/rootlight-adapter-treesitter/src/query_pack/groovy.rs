//! Maps authored Groovy nodes to parser-independent structural roles.
//! Declaration ownership follows native fields, never descendant-name heuristics.

use super::StructuralRole;
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use std::collections::HashMap;
use tree_sitter::Node;

/// Finds the single leading package that can own the rest of this source unit.
///
/// # Errors
/// Returns cancellation while scanning direct children; malformed, late or
/// repeated package headers are not promoted to compilation-unit containers.
pub(super) fn leading_package<'tree>(
    root: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    cancellation.check()?;
    let mut package = None;
    let mut seen_statement = false;
    let mut cursor = root.walk();
    for (index, child) in root.named_children(&mut cursor).enumerate() {
        if index % 64 == 0 {
            cancellation.check()?;
        }
        match child.kind() {
            "line_comment" | "block_comment" | "groovydoc_comment" | "shebang" => {}
            "package_declaration" => {
                if seen_statement || child.has_error() || child.is_missing() {
                    return Ok(None);
                }
                package = Some(child);
                seen_statement = true;
            }
            _ => seen_statement = true,
        }
    }
    Ok(package)
}

/// Per-scan member classification shares a declaration header across all its bindings.
#[derive(Default)]
pub(super) struct MemberKinds {
    declarations: HashMap<usize, &'static str>,
}

impl MemberKinds {
    /// Classifies a native field declarator without repeating annotation/modifier scans.
    ///
    /// # Errors
    /// Returns cancellation, allocation, or invalid native-owner errors.
    pub(super) fn kind(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<&'static str, AdapterError> {
        cancellation.check()?;
        let declaration = node
            .parent()
            .filter(|parent| parent.kind() == "field_declaration")
            .ok_or_else(|| super::query_failure("query-groovy-field-owner"))?;
        if let Some(kind) = self.declarations.get(&declaration.id()) {
            return Ok(*kind);
        }
        let kind = member_kind(declaration, cancellation)?;
        self.declarations
            .try_reserve(1)
            .map_err(|_| super::query_failure("query-groovy-member-allocation"))?;
        self.declarations.insert(declaration.id(), kind);
        Ok(kind)
    }
}

fn member_kind(
    declaration: Node<'_>,
    cancellation: &Cancellation,
) -> Result<&'static str, AdapterError> {
    if declaration
        .parent()
        .and_then(|body| body.parent())
        .is_some_and(|owner| {
            matches!(
                owner.kind(),
                "interface_declaration" | "annotation_type_declaration"
            )
        })
    {
        return Ok("groovy.field");
    }
    let mut cursor = declaration.walk();
    for (index, child) in declaration.children(&mut cursor).enumerate() {
        if index % 64 == 0 {
            cancellation.check()?;
        }
        match child.kind() {
            "public" | "protected" | "private" => return Ok("groovy.field"),
            "variable_declarator" => break,
            _ => {}
        }
    }
    // Static/final modifiers do not suppress Groovy's implicit property accessors.
    // Only authored declarations are emitted; generated accessors have no source span.
    Ok("groovy.property")
}

pub(super) fn syntax(node: Node<'_>, role: StructuralRole) -> Option<&'static str> {
    if node.kind() == "identifier"
        && node.parent().is_some_and(|parent| {
            parent.kind() == "import_declaration"
                && parent.child_by_field_name("alias") == Some(node)
        })
    {
        return Some("groovy.import_alias");
    }
    if role == StructuralRole::Expression
        && node.parent().is_some_and(|parent| {
            parent.kind() == "for_in_statement" && parent.child_by_field_name("value") == Some(node)
        })
    {
        return Some("groovy.iterable");
    }
    if role == StructuralRole::Signature {
        return Some("groovy.parameters");
    }
    Some(match node.kind() {
        "source_file" => "groovy.file",
        "package_declaration" => "groovy.package",
        "qualified_name" => "groovy.qualified_identifier",
        "class_declaration" => "groovy.class",
        "interface_declaration" | "annotation_type_declaration" => "groovy.interface",
        "trait_declaration" => "groovy.trait",
        "record_declaration" => "groovy.record",
        "enum_declaration" => "groovy.enum",
        "enum_constant" => "groovy.constant",
        // Script methods are file-level functions; lowering refines members using their type owner.
        "method_declaration" => "groovy.function",
        "constructor_declaration" => "groovy.constructor",
        "variable_declarator" => {
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "field_declaration")
            {
                "groovy.field"
            } else {
                "groovy.variable"
            }
        }
        "formal_parameter" | "closure_parameter" | "catch_formal_parameter" => "groovy.parameter",
        "resource" => "groovy.variable",
        "type_parameter" => "groovy.type_parameter",
        "record_component" => "groovy.property",
        "identifier"
            if role == StructuralRole::Declaration
                && node.parent().is_some_and(|parent| {
                    parent.kind() == "for_in_statement"
                        && parent.child_by_field_name("variable") == Some(node)
                }) =>
        {
            "groovy.variable"
        }
        "identifier" => "groovy.identifier",
        "type_identifier" => "groovy.type_identifier",
        "field_declaration" => "groovy.member",
        "static_initializer" => "groovy.static_context",
        "quoted_identifier" => "groovy.quoted_identifier",
        "import_declaration" => "groovy.import",
        "method_invocation" => "groovy.call",
        "block" => "groovy.block",
        "class_body" | "enum_body" => "groovy.type_body",
        "for_statement" => "groovy.for",
        "for_in_statement" => "groovy.enhanced_for",
        "local_variable_declaration" => "groovy.local_binding",
        "multi_assignment_declaration" => "groovy.destructuring",
        "catch_clause" => "groovy.catch",
        "try_statement" => "groovy.try",
        "closure" | "gstring_closure" if role == StructuralRole::Declaration => {
            "groovy.anonymous_function"
        }
        "closure" | "gstring_closure" => "groovy.closure",
        "line_comment" => "groovy.line_comment",
        "block_comment" => "groovy.block_comment",
        "string_literal" => "groovy.string",
        _ => return None,
    })
}

/// Distinguishes variable expressions from literal labels and member spelling.
pub(super) fn nonlexical_identifier(node: Node<'_>) -> bool {
    if node.kind() != "identifier" {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    let field = match parent.kind() {
        "field_access" | "direct_field_access_expression" => "field",
        "safe_navigation_expression" | "safe_chain_dot_expression" | "spread_dot_expression" => {
            "property"
        }
        "method_pointer_expression" => "method",
        "method_reference_expression" => "name",
        "method_invocation" => "function",
        "command_chain" => "receiver",
        "command_link" => "name",
        "command_suffix" => "member",
        "map_entry" | "named_argument" => "key",
        "labeled_statement" | "break_statement" | "continue_statement" => "label",
        "qualified_name" => return true,
        "import_declaration" => "alias",
        _ => return false,
    };
    parent.child_by_field_name(field) == Some(node)
}

/// Preserves import namespaces, interpolation names and native binding boundaries.
///
/// # Errors
/// Returns cancellation or an incomplete native control/header error.
pub(super) fn adjust_candidate(
    node: Node<'_>,
    candidate: &mut super::QueryCandidate,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    if candidate.role == StructuralRole::Reference
        && node.kind() == "qualified_name"
        && let Some(import) = node.parent().filter(|parent| {
            parent.kind() == "import_declaration"
                && parent.child_by_field_name("name") == Some(node)
        })
    {
        // Only direct anonymous tokens distinguish the import namespace. Text
        // inside an annotation or comment must not become a static/star marker.
        let mut is_static = false;
        let mut is_star = false;
        let mut cursor = import.walk();
        for (index, child) in import.children(&mut cursor).enumerate() {
            if index % 64 == 0 {
                cancellation.check()?;
            }
            match child.kind() {
                "static" => is_static = true,
                ".*" => is_star = true,
                _ => {}
            }
        }
        candidate.syntax = match (is_static, is_star) {
            (false, false) => "groovy.type_import",
            (false, true) => "groovy.package_star_import",
            (true, false) => "groovy.static_import",
            (true, true) => "groovy.static_star_import",
        };
    }
    if candidate.role == StructuralRole::Reference
        && node.kind() == "type_identifier"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "qualified_type")
    {
        candidate.syntax = "groovy.qualified_type_identifier";
    }
    if candidate.role == StructuralRole::Reference
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "gstring_dollar_interpolation")
    {
        let text = source
            .get(candidate.start..candidate.end)
            .and_then(|text| text.strip_prefix(b"$"))
            .ok_or_else(|| super::query_failure("query-groovy-interpolation-name"))?;
        let length = text
            .iter()
            .position(|byte| *byte == b'.')
            .unwrap_or(text.len());
        if length == 0 {
            return Err(super::query_failure("query-groovy-interpolation-name"));
        }
        // The native token includes '$' and optional property suffixes; only its
        // first identifier is a lexical read, with exactly those authored bytes.
        candidate.start = candidate
            .start
            .checked_add(1)
            .ok_or_else(|| super::query_failure("query-groovy-interpolation-name"))?;
        candidate.end = candidate
            .start
            .checked_add(length)
            .ok_or_else(|| super::query_failure("query-groovy-interpolation-name"))?;
    }
    if candidate.role != StructuralRole::Scope {
        return Ok(());
    }
    if matches!(
        node.kind(),
        "class_declaration"
            | "interface_declaration"
            | "annotation_type_declaration"
            | "trait_declaration"
            | "record_declaration"
            | "enum_declaration"
            | "method_declaration"
            | "field_declaration"
    ) && static_context(node, cancellation)?
    {
        candidate.syntax = if node.kind() == "method_declaration" {
            "groovy.static_function"
        } else {
            "groovy.static_context"
        };
    }
    if node.kind() == "try_statement" {
        // Resource bindings end with the try body, not with its catch/finally clauses.
        candidate.end = node
            .child_by_field_name("body")
            .ok_or_else(|| super::query_failure("query-groovy-try-body"))?
            .end_byte();
    }
    if matches!(node.kind(), "closure" | "gstring_closure") {
        let end = closure_header_end(node, cancellation)?;
        if node
            .child(0)
            .is_some_and(|opener| opener.kind() == "{" && end == opener.end_byte())
        {
            candidate.syntax = "groovy.implicit_closure";
        }
    }
    cancellation.check()?;
    Ok(())
}

fn static_context(node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
    // Groovy resolves enclosing type parameters in nested interfaces, traits,
    // annotations and records; Java's implicit-static rule cannot classify them.
    if node.kind() == "enum_declaration" {
        return Ok(true);
    }
    let mut cursor = node.walk();
    for (index, child) in node.children(&mut cursor).enumerate() {
        if index % 64 == 0 {
            cancellation.check()?;
        }
        if child.kind() == "static" {
            return Ok(true);
        }
        if matches!(
            child.kind(),
            "identifier"
                | "type_identifier"
                | "type_parameters"
                | "class_body"
                | "block"
                | "variable_declarator"
        ) {
            break;
        }
    }
    Ok(node
        .parent()
        .and_then(|body| body.parent())
        .is_some_and(|owner| {
            (node.kind() == "class_declaration"
                && matches!(
                    owner.kind(),
                    "interface_declaration" | "annotation_type_declaration" | "trait_declaration"
                ))
                || (node.kind() == "field_declaration"
                    && matches!(
                        owner.kind(),
                        "interface_declaration" | "annotation_type_declaration"
                    ))
        }))
}

/// Selects only the native closure header, including an explicit empty parameter list.
///
/// # Errors
/// Returns cancellation or an incomplete native header error; never scans body text.
pub(super) fn closure_header_end(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let opener = node.child(0).filter(|child| child.kind() == "{");
    let mut cursor = node.walk();
    for (index, child) in node.named_children(&mut cursor).enumerate() {
        if index % 64 == 0 {
            cancellation.check()?;
        }
        match child.kind() {
            "closure_parameters" => return Ok(child.end_byte()),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
    }
    // No arrow means an implicit parameter, not a body-derived signature.
    opener
        .map(|node| node.end_byte())
        .ok_or_else(|| super::query_failure("query-groovy-closure-header"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_context_requires_one_leading_header_and_honors_cancellation() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
            .unwrap();
        for (source, expected) in [
            ("package sample\nclass Meter {}", Some("package sample")),
            (
                "// header\npackage sample\nclass Meter {}",
                Some("package sample"),
            ),
            ("package sample\npackage repeated\nclass Meter {}", None),
            ("class Before {}\npackage late", None),
            ("", None),
        ] {
            let tree = parser.parse(source, None).unwrap();
            let cancellation = Cancellation::new();
            let package = leading_package(tree.root_node(), &cancellation).unwrap();
            assert_eq!(
                package.map(|node| node.utf8_text(source.as_bytes()).unwrap()),
                expected
            );
            cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            assert!(leading_package(tree.root_node(), &cancellation).is_err());
        }
    }

    #[test]
    fn member_headers_are_shared_and_cancellable_without_text_keyword_matching() {
        let source = b"class Holder { /* private */ @Mark(\"public\") String first, second }";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let body = tree
            .root_node()
            .named_child(0)
            .unwrap()
            .child_by_field_name("body")
            .unwrap();
        let mut cursor = body.walk();
        let declaration = body
            .named_children(&mut cursor)
            .find(|node| node.kind() == "field_declaration")
            .unwrap();
        let mut cursor = declaration.walk();
        let bindings: Vec<_> = declaration
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "variable_declarator")
            .collect();
        assert_eq!(bindings.len(), 2);
        let cancellation = Cancellation::new();
        let mut cache = MemberKinds::default();
        for _ in 0..128 {
            for binding in &bindings {
                assert_eq!(
                    cache.kind(*binding, &cancellation).unwrap(),
                    "groovy.property"
                );
            }
        }
        assert_eq!(cache.declarations.len(), 1);
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(cache.kind(bindings[0], &cancellation).is_err());
        assert!(
            MemberKinds::default()
                .kind(bindings[0], &cancellation)
                .is_err()
        );
    }
}
