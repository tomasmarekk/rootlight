//! Shared parser-independent structural fact classification.

use std::cmp::Ordering;

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

/// Returns the grammar-reviewed base entity kind for one structural syntax fact.
///
/// Contextual lowering may refine a function to a method when its parent is a
/// type or a Rust `impl`. Whole-project analysis uses the same base classifier
/// before applying that identical contextual refinement.
#[must_use]
pub fn structural_entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    let label = fact.syntax_kind().as_str();
    match fact.kind() {
        SyntaxFactKind::Module => Some(EntityKind::Module),
        SyntaxFactKind::Declaration if label == "java.annotation.declaration" => {
            Some(EntityKind::Interface)
        }
        SyntaxFactKind::Declaration if label == "java.annotation_element.declaration" => {
            Some(EntityKind::Method)
        }
        SyntaxFactKind::Declaration if label.contains("constructor") => {
            Some(EntityKind::Constructor)
        }
        SyntaxFactKind::Declaration if label.contains("record") => Some(EntityKind::Struct),
        SyntaxFactKind::Declaration if label.contains("method") => Some(EntityKind::Method),
        SyntaxFactKind::Declaration if label.contains("function") => Some(EntityKind::Function),
        SyntaxFactKind::Declaration if label.contains("class") => Some(EntityKind::Class),
        SyntaxFactKind::Declaration if label.contains("struct") => Some(EntityKind::Struct),
        SyntaxFactKind::Declaration if label.contains("enum") => Some(EntityKind::Enum),
        SyntaxFactKind::Declaration if label.contains("trait") => Some(EntityKind::Trait),
        SyntaxFactKind::Declaration if label.contains("interface") => Some(EntityKind::Interface),
        SyntaxFactKind::Declaration
            if label.contains("type_alias")
                || label.contains("type_item")
                || label.contains("type.declaration") =>
        {
            Some(EntityKind::TypeAlias)
        }
        SyntaxFactKind::Declaration
            if label.contains("constant")
                || label.contains("const_item")
                || label.contains("const.declaration") =>
        {
            Some(EntityKind::Constant)
        }
        SyntaxFactKind::Declaration
            if label.contains("static_item") || label.contains("static.declaration") =>
        {
            Some(EntityKind::Variable)
        }
        SyntaxFactKind::Declaration if label.contains("field") => Some(EntityKind::Field),
        SyntaxFactKind::Declaration if label.contains("parameter") => Some(EntityKind::Parameter),
        SyntaxFactKind::Declaration if label.contains("variable") => Some(EntityKind::Variable),
        _ => None,
    }
}

/// Refines a base structural kind using only the captured declaration source.
///
/// The Go grammar represents aliases, structs, and interfaces with one
/// `type_spec` node. Its declaration text is therefore the only reviewed local
/// evidence that can distinguish those kinds without repository execution.
#[must_use]
pub fn structural_entity_kind_from_source(
    fact: &SyntaxFact,
    declaration: &str,
) -> Option<EntityKind> {
    let kind = structural_entity_kind(fact)?;
    if fact.syntax_kind().as_str() != "go.type.declaration" {
        return Some(kind);
    }
    let header = declaration.trim_start();
    if header.contains("interface") {
        Some(EntityKind::Interface)
    } else if header.contains("struct") {
        Some(EntityKind::Struct)
    } else {
        Some(kind)
    }
}

/// Returns the bounded name accepted from a reviewed structural capture.
///
/// Project analyzers use the same boundary when preserving structural
/// declarations so grammar-valid scoped names cannot disappear during
/// semantic refinement.
#[must_use]
pub fn structural_captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    let candidate = text.trim();
    (!candidate.is_empty()
        && candidate.len() <= maximum_bytes
        && candidate.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(character, '/' | '\\' | '(' | ')' | '{' | '}' | '[' | ']')
        }))
    .then_some(candidate)
}

/// Orders syntax facts exactly as structural declaration association consumes them.
///
/// Whole-project analyzers must use this order before resolving nearest
/// declarations. Parser-local identifiers are intentionally excluded because
/// their assignment order is not part of the structural identity contract.
#[must_use]
pub fn structural_syntax_fact_order(left: &SyntaxFact, right: &SyntaxFact) -> Ordering {
    (
        left.depth(),
        left.span().start_byte(),
        left.span().end_byte(),
        syntax_fact_kind_tag(left.kind()),
        left.syntax_kind().as_str(),
    )
        .cmp(&(
            right.depth(),
            right.span().start_byte(),
            right.span().end_byte(),
            syntax_fact_kind_tag(right.kind()),
            right.syntax_kind().as_str(),
        ))
}

const fn syntax_fact_kind_tag(kind: SyntaxFactKind) -> u8 {
    match kind {
        SyntaxFactKind::Root => 1,
        SyntaxFactKind::Module => 2,
        SyntaxFactKind::Declaration => 3,
        SyntaxFactKind::Signature => 4,
        SyntaxFactKind::Import => 5,
        SyntaxFactKind::Scope => 6,
        SyntaxFactKind::Occurrence => 7,
        SyntaxFactKind::Comment => 8,
        SyntaxFactKind::StringLiteral => 9,
        SyntaxFactKind::EmbeddedRegion => 10,
        SyntaxFactKind::ErrorRecovery => 11,
    }
}
