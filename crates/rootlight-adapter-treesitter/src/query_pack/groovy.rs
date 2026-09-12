//! Maps authored Groovy nodes to parser-independent structural roles.
//! Declaration ownership follows native fields, never descendant-name heuristics.

use super::StructuralRole;
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use std::collections::HashMap;
use tree_sitter::Node;

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
        "formal_parameter" | "closure_parameter" => "groovy.parameter",
        "identifier" => "groovy.identifier",
        "type_identifier" => "groovy.type_identifier",
        "quoted_identifier" => "groovy.quoted_identifier",
        "import_declaration" => "groovy.import",
        "method_invocation" => "groovy.call",
        "block" | "class_body" | "enum_body" => "groovy.block",
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
