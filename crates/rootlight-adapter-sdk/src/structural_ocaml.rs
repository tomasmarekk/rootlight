//! OCaml identities preserve written names, not inferred types or module bindings.
//! Native callers prove exact identifier leaves and implementation/interface context.
//! Abstract type source markers never acquire the shared substring classifier.

use rootlight_ir::EntityKind;
use rootlight_unicode::{is_xid_continue, is_xid_start};

use crate::{SyntaxFact, SyntaxFactKind};

/// Classifies only exact written owner labels in the two OCaml namespaces.
pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "ocaml.file.module" | "ocaml_interface.file.module") => {
            Some(EntityKind::Module)
        }
        (
            SyntaxFactKind::Declaration,
            "ocaml.function.declaration" | "ocaml_interface.function.declaration",
        ) => Some(EntityKind::Function),
        (
            SyntaxFactKind::Declaration,
            "ocaml.variable.declaration" | "ocaml_interface.variable.declaration",
        ) => Some(EntityKind::Variable),
        (SyntaxFactKind::Declaration, "ocaml.parameter.declaration") => Some(EntityKind::Parameter),
        (
            SyntaxFactKind::Declaration,
            "ocaml.struct.declaration" | "ocaml_interface.struct.declaration",
        ) => Some(EntityKind::Struct),
        (
            SyntaxFactKind::Declaration,
            "ocaml.enum.declaration" | "ocaml_interface.enum.declaration",
        ) => Some(EntityKind::Enum),
        (
            SyntaxFactKind::Declaration,
            "ocaml.type_alias.declaration" | "ocaml_interface.type_alias.declaration",
        ) => Some(EntityKind::TypeAlias),
        (
            SyntaxFactKind::Declaration,
            "ocaml.module.declaration"
            | "ocaml_interface.module.declaration"
            | "ocaml.module_type.declaration"
            | "ocaml_interface.module_type.declaration",
        ) => Some(EntityKind::Module),
        _ => None,
    }
}

/// Borrows a bounded complete identifier without normalization or extraction.
///
/// Native evidence owns the grammar's precise Ll/Lu category and keyword rules;
/// this lexical guard requires case-bearing XID spelling and preserves apostrophes.
pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    if text.len() > maximum_bytes || text == "_" {
        return None;
    }
    let mut characters = text.chars();
    let first = characters.next()?;
    if first != '_' && (!is_xid_start(first) || !first.is_lowercase() && !first.is_uppercase()) {
        return None;
    }
    characters
        .all(|character| is_xid_continue(character) || character == '\'')
        .then_some(text)
}

fn value_path(text: &str, maximum_bytes: usize) -> Option<&str> {
    if text.len() > maximum_bytes {
        return None;
    }
    let mut parts = text.split('.').peekable();
    while let Some(part) = parts.next() {
        let _ = captured_name(part, maximum_bytes)?;
        let first = part.chars().next()?;
        if if parts.peek().is_some() {
            !first.is_uppercase()
        } else {
            first != '_' && !first.is_lowercase()
        } {
            return None;
        }
    }
    Some(text)
}

/// Borrows names only for exact occurrence labels in the matching physical profile.
///
/// Implementation paths retain written qualifiers; interface definitions cannot
/// acquire application or read roles from the implementation namespace.
pub(super) fn captured_name_for_fact<'a>(
    language: &str,
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    if fact.kind() != SyntaxFactKind::Occurrence {
        return None;
    }
    match (language, fact.syntax_kind().as_str()) {
        ("ocaml", "ocaml.name.definition")
        | ("ocaml-interface", "ocaml_interface.name.definition") => {
            captured_name(text, maximum_bytes)
        }
        ("ocaml", "ocaml.application.call" | "ocaml.value.reference") => {
            value_path(text, maximum_bytes)
        }
        ("ocaml", "ocaml.match_formal.reference" | "ocaml.direct_file_call.call") => {
            captured_name(text, maximum_bytes)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    use super::*;
    use crate::{
        SyntaxKindLabel, structural_captured_name_for_fact, structural_captured_name_for_language,
        structural_entity_kind,
    };

    const ROLES: [SyntaxFactKind; 11] = [
        SyntaxFactKind::Root,
        SyntaxFactKind::Module,
        SyntaxFactKind::Declaration,
        SyntaxFactKind::Signature,
        SyntaxFactKind::Import,
        SyntaxFactKind::Scope,
        SyntaxFactKind::Occurrence,
        SyntaxFactKind::Comment,
        SyntaxFactKind::StringLiteral,
        SyntaxFactKind::EmbeddedRegion,
        SyntaxFactKind::ErrorRecovery,
    ];

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([7; 20]), 0, 8).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn ocaml_kinds_require_exact_written_roles_and_namespaces() {
        for (role, label, expected) in [
            (
                SyntaxFactKind::Module,
                "ocaml.file.module",
                EntityKind::Module,
            ),
            (
                SyntaxFactKind::Module,
                "ocaml_interface.file.module",
                EntityKind::Module,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml.function.declaration",
                EntityKind::Function,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml_interface.function.declaration",
                EntityKind::Function,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml.variable.declaration",
                EntityKind::Variable,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml_interface.variable.declaration",
                EntityKind::Variable,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml.parameter.declaration",
                EntityKind::Parameter,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml.struct.declaration",
                EntityKind::Struct,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml_interface.struct.declaration",
                EntityKind::Struct,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml.enum.declaration",
                EntityKind::Enum,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml_interface.enum.declaration",
                EntityKind::Enum,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml.type_alias.declaration",
                EntityKind::TypeAlias,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml_interface.type_alias.declaration",
                EntityKind::TypeAlias,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml.module.declaration",
                EntityKind::Module,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml_interface.module.declaration",
                EntityKind::Module,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml.module_type.declaration",
                EntityKind::Module,
            ),
            (
                SyntaxFactKind::Declaration,
                "ocaml_interface.module_type.declaration",
                EntityKind::Module,
            ),
        ] {
            for kind in ROLES {
                assert_eq!(
                    structural_entity_kind(&fact(kind, label)),
                    (kind == role).then_some(expected)
                );
                assert!(structural_entity_kind(&fact(kind, &format!("{label}.extra"))).is_none());
            }
        }
        for label in [
            "ocaml.type.declaration",
            "ocaml_interface.abstract_type.declaration",
            "ocaml_interface.parameter.declaration",
            "ocaml.unknown_function.declaration",
            "ocaml.unsupported_type.signature",
        ] {
            for kind in ROLES {
                assert!(
                    structural_entity_kind(&fact(kind, label)).is_none(),
                    "{kind:?} {label}"
                );
            }
        }
    }

    #[test]
    fn ocaml_identifiers_preserve_case_apostrophes_unicode_and_byte_bounds() {
        for name in [
            "item",
            "Worker",
            "_item",
            "item'",
            "α",
            "État",
            "a\u{301}",
            "module_name2",
        ] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert!(captured_name(name, name.len() - 1).is_none());
            for language in ["ocaml", "ocaml-interface"] {
                assert_eq!(
                    structural_captured_name_for_language(language, name, name.len()),
                    Some(Cow::Borrowed(name))
                );
            }
        }
        for name in [
            "",
            "_",
            "1item",
            "'item",
            "\u{301}item",
            "雪",
            " item",
            "item ",
            "two names",
            "item\0",
            "R.item",
            "(+)",
            "item\n",
        ] {
            assert!(captured_name(name, 128).is_none(), "{name:?}");
        }
        assert_ne!(captured_name("Item", 4), captured_name("item", 4));
    }

    #[test]
    fn ocaml_occurrence_names_fence_profiles_roles_and_qualified_paths() {
        for (language, label, name) in [
            ("ocaml", "ocaml.name.definition", "Worker"),
            ("ocaml-interface", "ocaml_interface.name.definition", "read"),
            ("ocaml", "ocaml.application.call", "Reader.read"),
            ("ocaml", "ocaml.value.reference", "Outer.Inner.item'"),
        ] {
            for role in ROLES {
                let expected = (role == SyntaxFactKind::Occurrence).then_some(Cow::Borrowed(name));
                assert_eq!(
                    structural_captured_name_for_fact(
                        language,
                        &fact(role, label),
                        name,
                        name.len()
                    ),
                    expected
                );
            }
            for foreign in [
                "rust",
                "ocaml-type",
                "OCaml",
                "",
                if language == "ocaml" {
                    "ocaml-interface"
                } else {
                    "ocaml"
                },
            ] {
                assert!(
                    structural_captured_name_for_fact(
                        foreign,
                        &fact(SyntaxFactKind::Occurrence, label),
                        name,
                        128
                    )
                    .is_none()
                );
            }
            assert!(
                structural_captured_name_for_fact(
                    language,
                    &fact(SyntaxFactKind::Occurrence, &format!("{label}.extra")),
                    name,
                    128
                )
                .is_none()
            );
        }
        for label in ["ocaml.application.call", "ocaml.value.reference"] {
            let occurrence = fact(SyntaxFactKind::Occurrence, label);
            for invalid in [
                "Reader.",
                ".item",
                "lower.item",
                "Reader..item",
                "Reader.Item",
                "R .item",
                "R. item",
                "(+)",
                "_",
            ] {
                assert!(
                    structural_captured_name_for_fact("ocaml", &occurrence, invalid, 128).is_none(),
                    "{invalid}"
                );
            }
        }
        assert!(
            structural_captured_name_for_fact(
                "ocaml",
                &fact(SyntaxFactKind::Occurrence, "ocaml.name.definition"),
                "R.item",
                128
            )
            .is_none()
        );
    }
}

#[cfg(test)]
mod written_binding_label_tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn source_certified_ocaml_names_require_exact_role_profile_and_whole_identifier() {
        for label in [
            "ocaml.match_formal.reference",
            "ocaml.direct_file_call.call",
        ] {
            let fact = |kind| {
                SyntaxFact::new(
                    1,
                    None,
                    kind,
                    SourceSpan::new(FileId::from_bytes([8; 20]), 0, 5).unwrap(),
                    0,
                    SyntaxKindLabel::new(label).unwrap(),
                )
            };
            let site = fact(SyntaxFactKind::Occurrence);
            assert_eq!(
                captured_name_for_fact("ocaml", &site, "value", 5),
                Some("value")
            );
            assert_eq!(
                captured_name_for_fact("ocaml", &site, "é雪", 5),
                Some("é雪")
            );
            assert!(captured_name_for_fact("ocaml", &site, "value", 4).is_none());
            assert!(captured_name_for_fact("ocaml-interface", &site, "value", 5).is_none());
            assert!(
                captured_name_for_fact("ocaml", &fact(SyntaxFactKind::Signature), "value", 5)
                    .is_none()
            );
            for raw in [
                "_",
                "Reader.value",
                "value suffix",
                "(value)",
                "value\\suffix",
            ] {
                assert!(
                    captured_name_for_fact("ocaml", &site, raw, 64).is_none(),
                    "{raw}"
                );
            }
        }
    }
}
