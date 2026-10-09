//! Common Lisp structural facts retain exact written kinds and symbol spelling.
//! Native callers must prove complete symbol captures and declaration contexts;
//! these classifications do not establish reader, package or runtime bindings.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

/// Classifies exact written declaration kinds without proving runtime identity.
pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "common_lisp.function.declaration"
        | "common_lisp.macro.declaration"
        | "common_lisp.generic_function.declaration" => Some(EntityKind::Function),
        "common_lisp.method.declaration" => Some(EntityKind::Method),
        "common_lisp.class.declaration" => Some(EntityKind::Class),
        "common_lisp.package.declaration" => Some(EntityKind::Namespace),
        "common_lisp.variable.declaration" => Some(EntityKind::Variable),
        "common_lisp.constant.declaration" => Some(EntityKind::Constant),
        "common_lisp.parameter.declaration" => Some(EntityKind::Parameter),
        _ => None,
    }
}

/// Retains one bounded, complete written symbol without interpreting its reader.
///
/// The native caller must independently prove a clean complete symbol capture:
/// numbers, strings, quoted data and arbitrary forms are not admitted by a name
/// field alone. This borrowed lexical boundary never decodes escapes, changes
/// case, checks package existence or equates separate written spellings.
pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum_bytes {
        return None;
    }
    let mut multiple_escape = false;
    let mut single_escape = false;
    for (offset, character) in text.char_indices() {
        if character == '\0' {
            return None;
        }
        if single_escape {
            single_escape = false;
            continue;
        }
        // Standard reader dispatch starts with '#'; inside an accumulated token
        // it is non-terminating. Retaining it does not prove readtable acceptance.
        // See https://www.lispworks.com/documentation/HyperSpec/Body/02_ad.htm.
        match character {
            '\\' => single_escape = true,
            '|' => multiple_escape = !multiple_escape,
            _ if !multiple_escape
                && (character.is_whitespace()
                    || matches!(character, '(' | ')' | '"' | ';' | '\'' | '`' | ',')
                    || offset == 0 && character == '#') =>
            {
                return None;
            }
            _ => {}
        }
    }
    (!multiple_escape && !single_escape).then_some(text)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use rootlight_ids::FileId;
    use rootlight_ir::{EntityKind, SourceSpan};

    use crate::{
        SyntaxFact, SyntaxFactKind, SyntaxKindLabel, structural_captured_name_for_fact,
        structural_captured_name_for_language, structural_entity_kind,
        structural_entity_kind_from_source,
    };

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn common_lisp_declarations_use_exact_reviewed_kinds() {
        for (label, expected) in [
            ("common_lisp.function.declaration", EntityKind::Function),
            ("common_lisp.macro.declaration", EntityKind::Function),
            (
                "common_lisp.generic_function.declaration",
                EntityKind::Function,
            ),
            ("common_lisp.method.declaration", EntityKind::Method),
            ("common_lisp.class.declaration", EntityKind::Class),
            ("common_lisp.package.declaration", EntityKind::Namespace),
            ("common_lisp.variable.declaration", EntityKind::Variable),
            ("common_lisp.constant.declaration", EntityKind::Constant),
            ("common_lisp.parameter.declaration", EntityKind::Parameter),
        ] {
            let declaration = fact(SyntaxFactKind::Declaration, label);
            assert_eq!(
                structural_entity_kind(&declaration),
                Some(expected),
                "{label}"
            );
            assert_eq!(
                structural_entity_kind_from_source(&declaration, "written source"),
                Some(expected),
                "{label}"
            );
        }
    }

    #[test]
    fn common_lisp_unknown_labels_and_wrong_roles_do_not_fall_through() {
        for label in [
            "common_lisp.unknown_function.declaration",
            "common_lisp.defparameter.declaration",
            "common_lisp.anonymous_function.declaration",
            "common_lisp.in_package.declaration",
            "common_lisp.name.definition",
            "common_lisp.function.declaration.extra",
        ] {
            assert_eq!(
                structural_entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                None,
                "{label}"
            );
        }
        for kind in [
            SyntaxFactKind::Root,
            SyntaxFactKind::Module,
            SyntaxFactKind::Scope,
            SyntaxFactKind::Occurrence,
        ] {
            assert_eq!(
                structural_entity_kind(&fact(kind, "common_lisp.function.declaration")),
                None,
                "{kind:?}"
            );
        }
        assert_eq!(
            structural_entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "rust.function.declaration"
            )),
            Some(EntityKind::Function)
        );
    }

    #[test]
    fn common_lisp_names_borrow_exact_written_symbol_spelling() {
        let definition = fact(SyntaxFactKind::Occurrence, "common_lisp.name.definition");
        for source in [
            "Mixed",
            "mixed",
            "|Mixed Name|",
            r"escaped\ name",
            r"escaped\|bar",
            "pkg:written",
            "pkg::internal",
            "|Mixed Package|::|Mixed Name|",
            r"|pipe\|name|",
            r"mixed|part with spaces|tail",
            r"name\(part\)",
            "escaped\\\tspace",
            "escaped\\\nspace",
            "|line\nname|",
            "|a(b);'`,\"#name|",
            "name#tail",
            ":written-package",
            "||",
            "/",
            "*state*",
            "+unit+",
            "λ",
        ] {
            assert_eq!(
                structural_captured_name_for_language("common-lisp", source, source.len()),
                Some(Cow::Borrowed(source)),
                "{source:?}"
            );
            assert_eq!(
                structural_captured_name_for_fact("common-lisp", &definition, source, source.len()),
                Some(Cow::Borrowed(source)),
                "{source:?}"
            );
            assert!(
                structural_captured_name_for_language("common-lisp", source, source.len() - 1)
                    .is_none(),
                "{source:?}"
            );
            assert!(
                structural_captured_name_for_fact(
                    "common-lisp",
                    &definition,
                    source,
                    source.len() - 1
                )
                .is_none(),
                "{source:?}"
            );
        }
    }

    #[test]
    fn common_lisp_malformed_names_never_use_generic_fallback() {
        for source in [
            "",
            " name",
            "name ",
            "two names",
            "name\n",
            "name\0tail",
            "|name\0tail|",
            "name\\\0tail",
            "|name",
            "name|",
            "|name|tail|",
            "name\\",
            "|name\\|",
            "(name)",
            "name;comment",
            "'name",
            "`name",
            ",name",
            "name\"tail",
            "#'name",
            "#name",
        ] {
            assert!(
                structural_captured_name_for_language("common-lisp", source, 128).is_none(),
                "{source:?}"
            );
            assert!(
                structural_captured_name_for_fact(
                    "common-lisp",
                    &fact(SyntaxFactKind::Occurrence, "common_lisp.name.definition"),
                    source,
                    128
                )
                .is_none(),
                "{source:?}"
            );
        }
    }

    #[test]
    fn common_lisp_name_admission_requires_reviewed_language_label_and_role() {
        let definition = fact(SyntaxFactKind::Occurrence, "common_lisp.name.definition");
        for language in ["rust", "scheme", "lisp", "commonlisp", "COMMON-LISP"] {
            assert!(
                structural_captured_name_for_fact(language, &definition, "name", 128).is_none(),
                "{language}"
            );
        }
        for label in [
            "common_lisp.name.reference",
            "common_lisp.name.definition.extra",
            "common_lisp.function.declaration",
            "rust.name.definition",
        ] {
            assert!(
                structural_captured_name_for_fact(
                    "common-lisp",
                    &fact(SyntaxFactKind::Occurrence, label),
                    "name",
                    128
                )
                .is_none(),
                "{label}"
            );
        }
        for kind in [
            SyntaxFactKind::Root,
            SyntaxFactKind::Module,
            SyntaxFactKind::Scope,
            SyntaxFactKind::Declaration,
        ] {
            assert!(
                structural_captured_name_for_fact(
                    "common-lisp",
                    &fact(kind, "common_lisp.name.definition"),
                    "name",
                    128
                )
                .is_none(),
                "{kind:?}"
            );
        }
        assert!(structural_captured_name_for_language("rust", "/", 128).is_none());
    }

    #[test]
    fn common_lisp_written_names_do_not_claim_reader_equivalence() {
        for (left, right) in [
            ("Mixed", "mixed"),
            ("name", "|name|"),
            ("name", r"\name"),
            ("pkg:name", "pkg::name"),
            ("pkg:name", "name"),
            ("λ", "Λ"),
        ] {
            let left = structural_captured_name_for_language("common-lisp", left, 128)
                .expect("left fixture is written symbol spelling");
            let right = structural_captured_name_for_language("common-lisp", right, 128)
                .expect("right fixture is written symbol spelling");
            assert_ne!(left, right);
        }
    }
}
