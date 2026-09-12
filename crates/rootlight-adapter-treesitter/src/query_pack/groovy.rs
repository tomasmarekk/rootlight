//! Maps authored Groovy nodes to parser-independent structural roles.
//! Declaration ownership follows native fields, never descendant-name heuristics.

use super::StructuralRole;
use tree_sitter::Node;

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
        "closure" => "groovy.closure",
        "line_comment" => "groovy.line_comment",
        "block_comment" => "groovy.block_comment",
        "string_literal" => "groovy.string",
        _ => return None,
    })
}
