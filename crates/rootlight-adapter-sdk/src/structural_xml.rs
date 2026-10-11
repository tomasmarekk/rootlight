//! XML names are written XML 1.0 fifth-edition Names, not namespace bindings.
//! Native callers prove complete captures, physical owners and source freshness.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "xml.file.module" | "dtd.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "xml.element.declaration") => Some(EntityKind::MarkupElement),
        (SyntaxFactKind::Declaration, "xml.attribute.declaration") => {
            Some(EntityKind::MarkupAttribute)
        }
        _ => None,
    }
}

/// Borrows one complete raw Name; neither a QName nor a namespace is inferred.
pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum_bytes {
        return None;
    }
    let mut characters = text.chars();
    (characters.next().is_some_and(name_start) && characters.all(name_char)).then_some(text)
}

fn name_start(character: char) -> bool {
    matches!(character,
        ':' | 'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}'
        | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}'
        | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}'
        | '\u{10000}'..='\u{EFFFF}')
}

fn name_char(character: char) -> bool {
    name_start(character)
        || matches!(character,
            '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}'
            | '\u{203F}'..='\u{2040}')
}

pub(super) fn captured_name_for_fact<'a>(
    language: &str,
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    if fact.kind() != SyntaxFactKind::Occurrence {
        return None;
    }
    let admitted = match language {
        "xml" => matches!(
            fact.syntax_kind().as_str(),
            "xml.name.definition"
                | "xml.end_name.reference"
                | "xml.entity_name.reference"
                | "xml.parameter_entity_name.reference"
                | "xml.dtd_declared_name.reference"
                | "xml.dtd_name.reference"
                | "xml.pi_target.reference"
        ),
        "dtd" => matches!(
            fact.syntax_kind().as_str(),
            "dtd.entity_name.reference"
                | "dtd.parameter_entity_name.reference"
                | "dtd.dtd_declared_name.reference"
                | "dtd.dtd_name.reference"
                | "dtd.pi_target.reference"
        ),
        _ => false,
    };
    admitted
        .then(|| captured_name(text, maximum_bytes))
        .flatten()
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
            SourceSpan::new(FileId::from_bytes([3; 20]), 0, 8).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn xml_kinds_require_exact_physical_role_and_namespace() {
        for (role, label, expected) in [
            (
                SyntaxFactKind::Module,
                "xml.file.module",
                EntityKind::Module,
            ),
            (
                SyntaxFactKind::Module,
                "dtd.file.module",
                EntityKind::Module,
            ),
            (
                SyntaxFactKind::Declaration,
                "xml.element.declaration",
                EntityKind::MarkupElement,
            ),
            (
                SyntaxFactKind::Declaration,
                "xml.attribute.declaration",
                EntityKind::MarkupAttribute,
            ),
        ] {
            assert_eq!(entity_kind(&fact(role, label)), Some(expected));
            assert!(entity_kind(&fact(SyntaxFactKind::Occurrence, label)).is_none());
            assert!(entity_kind(&fact(role, &format!("{label}.extra"))).is_none());
        }
        for label in [
            "xml.namespace.declaration",
            "dtd.entity.declaration",
            "dtd.element.declaration",
            "xml.element.module",
            "html.element.declaration",
        ] {
            assert!(entity_kind(&fact(SyntaxFactKind::Declaration, label)).is_none());
        }
    }

    #[test]
    fn xml_raw_names_preserve_case_prefix_and_unicode_bytes() {
        for name in [
            "p:Root", "p:Value", "xmlns:p", "À·̀", "Ϳ‿", "𐀀́", "a:b:c", ":",
        ] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert!(captured_name(name, name.len() - 1).is_none());
        }
        for name in [
            "",
            " Root",
            "Root ",
            "1root",
            "̀root",
            ";root",
            "\u{F0000}",
            "a\0b",
            "a/b",
            "a&b",
            "<root>",
            "a\n",
            "a\u{200B}",
        ] {
            assert!(captured_name(name, 128).is_none(), "{name:?}");
        }
        assert!(captured_name("a", 0).is_none());
        assert_eq!(captured_name("a", 1), Some("a"));
    }

    #[test]
    fn xml_fact_names_do_not_grant_foreign_or_generic_fallback() {
        for (language, label) in [
            ("xml", "xml.name.definition"),
            ("xml", "xml.end_name.reference"),
            ("xml", "xml.pi_target.reference"),
            ("dtd", "dtd.dtd_declared_name.reference"),
            ("dtd", "dtd.entity_name.reference"),
        ] {
            let occurrence = fact(SyntaxFactKind::Occurrence, label);
            assert_eq!(
                captured_name_for_fact(language, &occurrence, "p:Root", 6),
                Some("p:Root")
            );
            assert!(captured_name_for_fact("html", &occurrence, "p:Root", 6).is_none());
            assert!(
                captured_name_for_fact(
                    language,
                    &fact(SyntaxFactKind::Declaration, label),
                    "p:Root",
                    6
                )
                .is_none()
            );
            assert!(
                captured_name_for_fact(
                    language,
                    &fact(SyntaxFactKind::Occurrence, &format!("{label}.extra")),
                    "p:Root",
                    6
                )
                .is_none()
            );
        }
        assert!(
            captured_name_for_fact(
                "dtd",
                &fact(SyntaxFactKind::Occurrence, "xml.name.definition"),
                "Root",
                4
            )
            .is_none()
        );
        assert!(
            captured_name_for_fact(
                "xml",
                &fact(SyntaxFactKind::Occurrence, "dtd.dtd_name.reference"),
                "Root",
                4
            )
            .is_none()
        );
    }
}
