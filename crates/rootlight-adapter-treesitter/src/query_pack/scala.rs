//! Scala source binding positions and private native syntax labels.
//! Pattern names are distinguished from extractor/type/member references before lowering.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;
use unicode_general_category::{GeneralCategory, get_general_category};

use super::StructuralRole;

pub(super) fn leading_package_end(
    root: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    cancellation.check()?;
    let mut end = None;
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        cancellation.check()?;
        match child.kind() {
            "comment" => {}
            "package_clause" if child.child_by_field_name("body").is_none() => {
                end = Some(child.end_byte());
            }
            _ => break,
        }
    }
    Ok(end)
}

pub(super) fn retain_capture(node: Node<'_>, role: StructuralRole, source: &[u8]) -> bool {
    if matches!(node.kind(), "identifier" | "operator_identifier") {
        match role {
            StructuralRole::Declaration => binding_syntax(node, source).is_some(),
            StructuralRole::Definition => {
                binding_syntax(node, source).is_some()
                    || node.parent().is_some_and(|parent| {
                        parent.child_by_field_name("name") == Some(node)
                            && declaration_syntax(parent, source).is_some()
                    })
            }
            _ => true,
        }
    } else {
        true
    }
}

fn binding_syntax(node: Node<'_>, source: &[u8]) -> Option<&'static str> {
    let mut child = node;
    while let Some(parent) = child.parent() {
        match parent.kind() {
            "parameter"
            | "class_parameter"
            | "binding"
            | "covariant_type_parameter"
            | "contravariant_type_parameter"
                if parent.child_by_field_name("name") == Some(child) =>
            {
                return Some("scala.parameter");
            }
            "type_parameters"
                if parent
                    .children_by_field_name("name", &mut parent.walk())
                    .any(|name| name == child) =>
            {
                return Some("scala.parameter");
            }
            "lambda_expression" if parent.child_by_field_name("parameters") == Some(child) => {
                return Some("scala.parameter");
            }
            "val_declaration" | "var_declaration"
                if parent
                    .children_by_field_name("name", &mut parent.walk())
                    .any(|name| name == child) =>
            {
                return Some("scala.variable");
            }
            "val_definition" | "var_definition"
                if parent.child_by_field_name("pattern") == Some(child) =>
            {
                return (child == node || variable_pattern(node, source))
                    .then_some("scala.variable");
            }
            "case_clause" if parent.child_by_field_name("pattern") == Some(child) => {
                return variable_pattern(node, source).then_some("scala.variable");
            }
            "enumerator" if parent.named_child(0) == Some(child) => {
                return variable_pattern(node, source).then_some("scala.variable");
            }
            "tuple_pattern" | "identifiers" | "alternative_pattern" | "named_tuple_pattern" => {}
            "named_pattern" if parent.named_child(1) == Some(child) => {}
            "infix_pattern"
                if parent.child_by_field_name("left") == Some(child)
                    || parent.child_by_field_name("right") == Some(child) => {}
            "capture_pattern" if parent.child_by_field_name("name") == Some(child) => {
                return variable_pattern(node, source).then_some("scala.variable");
            }
            "capture_pattern" | "typed_pattern" | "repeat_pattern"
                if parent.child_by_field_name("pattern") == Some(child) => {}
            "case_class_pattern"
                if parent
                    .children_by_field_name("pattern", &mut parent.walk())
                    .any(|pattern| pattern == child) => {}
            _ => return None,
        }
        child = parent;
    }
    None
}

fn variable_pattern(node: Node<'_>, source: &[u8]) -> bool {
    // SLS lexical/pattern rules distinguish binders from stable names such as None.
    // Letter numerals remain constants even when Unicode marks them lowercase.
    // https://www.scala-lang.org/files/archive/spec/2.13/01-lexical-syntax.html
    node.utf8_text(source)
        .ok()
        .and_then(|name| name.chars().next())
        .is_some_and(|first| {
            first == '_'
                || (first.is_lowercase()
                    && get_general_category(first) != GeneralCategory::LetterNumber)
        })
}

pub(super) fn declaration_syntax(node: Node<'_>, source: &[u8]) -> Option<&'static str> {
    if matches!(node.kind(), "identifier" | "operator_identifier") {
        return binding_syntax(node, source);
    }
    canonical_syntax(node.kind())
}

pub(super) fn canonical_syntax(native: &str) -> Option<&'static str> {
    Some(match native {
        "compilation_unit" => "scala.file",
        "package_clause" => "scala.package",
        "package_object" => "scala.package_object",
        "class_definition" | "full_enum_case" => "scala.class",
        "object_definition" => "scala.object",
        "trait_definition" => "scala.trait",
        "enum_definition" => "scala.enum",
        "simple_enum_case" => "scala.enum_value",
        "function_definition" | "function_declaration" => "scala.function",
        "given_definition" => "scala.given_variable",
        "type_definition" => "scala.type",
        "extension_definition" => "scala.extension",
        "lambda_expression" => "scala.lambda",
        "block" | "indented_block" => "scala.block",
        "case_clause" => "scala.case",
        "for_expression" => "scala.for",
        "identifier" | "operator_identifier" | "package_identifier" => "scala.identifier",
        "type_identifier" | "stable_type_identifier" => "scala.type_name",
        "import_declaration" | "export_declaration" => "scala.import",
        "comment" => "scala.comment",
        "string" => "scala.string",
        _ => return None,
    })
}
