//! WGSL facts retain written physical owners and raw Unicode 16 names.
//! The native adapter proves complete captures and their source context;
//! these helpers do not validate shaders, evaluate values or resolve bindings.

use rootlight_ir::EntityKind;
use rootlight_unicode::{is_xid_continue, is_xid_start};

use crate::{SyntaxFact, SyntaxFactKind};

/// Classifies only exact written declaration labels and the physical file owner.
pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "wgsl.file.module" {
        return Some(EntityKind::Module);
    }
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "wgsl.function.declaration" => Some(EntityKind::Function),
        "wgsl.struct.declaration" => Some(EntityKind::Struct),
        "wgsl.type_alias.declaration" => Some(EntityKind::TypeAlias),
        "wgsl.variable.declaration" | "wgsl.let.declaration" | "wgsl.override.declaration" => {
            Some(EntityKind::Variable)
        }
        "wgsl.constant.declaration" => Some(EntityKind::Constant),
        "wgsl.parameter.declaration" => Some(EntityKind::Parameter),
        "wgsl.field.declaration" => Some(EntityKind::Field),
        _ => None,
    }
}

/// Borrows one complete name matching the retained Unicode 16 lexical pattern.
///
/// The caller proves native capture context. This does not determine reserved
/// words, shader validity or bindings, and never folds or normalizes spelling.
pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    if text.len() > maximum_bytes {
        return None;
    }
    let mut characters = text.chars();
    let first = characters.next()?;
    if !is_xid_start(first) && first != '_' {
        return None;
    }
    // The native grammar admits an underscore only with another character.
    if first == '_' && characters.as_str().is_empty() {
        return None;
    }
    characters.all(is_xid_continue).then_some(text)
}

/// Borrows names only for exact definition and unresolved source-use labels.
pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    if fact.kind() != SyntaxFactKind::Occurrence {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "wgsl.name.definition"
        | "wgsl.identifier.reference"
        | "wgsl.type.reference"
        | "wgsl.member.reference"
        | "wgsl.unproven_call.reference" => captured_name(text, maximum_bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    use super::*;
    use crate::SyntaxKindLabel;

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([8; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn written_owner_kinds_do_not_infer_values_or_pipeline_bindings() {
        for (label, expected) in [
            ("wgsl.function.declaration", EntityKind::Function),
            ("wgsl.struct.declaration", EntityKind::Struct),
            ("wgsl.type_alias.declaration", EntityKind::TypeAlias),
            ("wgsl.variable.declaration", EntityKind::Variable),
            ("wgsl.let.declaration", EntityKind::Variable),
            ("wgsl.override.declaration", EntityKind::Variable),
            ("wgsl.constant.declaration", EntityKind::Constant),
            ("wgsl.parameter.declaration", EntityKind::Parameter),
            ("wgsl.field.declaration", EntityKind::Field),
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                Some(expected)
            );
            assert_eq!(entity_kind(&fact(SyntaxFactKind::Occurrence, label)), None);
            assert_eq!(entity_kind(&fact(SyntaxFactKind::Signature, label)), None);
        }
        assert_eq!(
            entity_kind(&fact(SyntaxFactKind::Module, "wgsl.file.module")),
            Some(EntityKind::Module)
        );
        assert_eq!(
            entity_kind(&fact(SyntaxFactKind::Declaration, "wgsl.file.module")),
            None
        );
        for label in [
            "wgsl.unknown.declaration",
            "wgsl.function.declaration.extra",
            "wgsl.interface.declaration",
            "hlsl.function.declaration",
        ] {
            assert_eq!(entity_kind(&fact(SyntaxFactKind::Declaration, label)), None);
        }
    }

    #[test]
    fn complete_raw_names_remain_borrowed_and_obey_utf8_byte_bounds() {
        for source in ["Written", "_value9", "Δvalue", "e\u{301}", "é", "__value"] {
            let admitted = captured_name(source, source.len()).unwrap();
            assert_eq!(admitted, source);
            assert!(std::ptr::eq(admitted.as_ptr(), source.as_ptr()));
            assert_eq!(captured_name(source, source.len() - 1), None);
        }
        assert_ne!(captured_name("é", 8), captured_name("e\u{301}", 8));
        assert_ne!(captured_name("Written", 8), captured_name("written", 8));
    }

    #[test]
    fn admission_never_trims_extracts_or_confuses_unicode_categories_with_xid() {
        for source in [
            "",
            "_",
            "9name",
            " name",
            "name ",
            "name\n",
            "name\0tail",
            "name.member",
            "name()",
            "name-name",
            "\u{301}name",
            "🛰name",
        ] {
            assert_eq!(captured_name(source, usize::MAX), None, "{source:?}");
        }
    }

    #[test]
    fn names_require_exact_occurrence_role_and_labels() {
        for label in [
            "wgsl.name.definition",
            "wgsl.identifier.reference",
            "wgsl.type.reference",
            "wgsl.member.reference",
            "wgsl.unproven_call.reference",
        ] {
            assert_eq!(
                captured_name_for_fact(&fact(SyntaxFactKind::Occurrence, label), "Δvalue", 16),
                Some("Δvalue")
            );
            assert_eq!(
                captured_name_for_fact(&fact(SyntaxFactKind::Declaration, label), "Δvalue", 16),
                None
            );
            assert_eq!(
                captured_name_for_fact(&fact(SyntaxFactKind::Occurrence, label), "Δvalue", 1),
                None
            );
        }
        for label in [
            "wgsl.unproven_call.call",
            "wgsl.unknown.reference",
            "wgsl.name.definition.extra",
            "hlsl.name.definition",
        ] {
            assert_eq!(
                captured_name_for_fact(&fact(SyntaxFactKind::Occurrence, label), "Written", 16),
                None
            );
        }
    }
}
