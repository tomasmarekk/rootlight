//! V facts preserve the reviewed grammar's written names and declaration roles.
//! Prefixes and operator spellings do not establish foreign or dispatch bindings.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "v.file.module")
        | (SyntaxFactKind::Declaration, "v.module.declaration") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "v.struct.declaration") => Some(EntityKind::Struct),
        (SyntaxFactKind::Declaration, "v.union.declaration") => Some(EntityKind::Union),
        (SyntaxFactKind::Declaration, "v.interface.declaration") => Some(EntityKind::Interface),
        (SyntaxFactKind::Declaration, "v.enum.declaration") => Some(EntityKind::Enum),
        (SyntaxFactKind::Declaration, "v.type.declaration") => Some(EntityKind::TypeAlias),
        (SyntaxFactKind::Declaration, "v.function.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "v.method.declaration") => Some(EntityKind::Method),
        (SyntaxFactKind::Declaration, "v.field.declaration") => Some(EntityKind::Field),
        (SyntaxFactKind::Declaration, "v.constant.declaration" | "v.enumerator.declaration") => {
            Some(EntityKind::Constant)
        }
        (SyntaxFactKind::Declaration, "v.variable.declaration") => Some(EntityKind::Variable),
        (SyntaxFactKind::Declaration, "v.parameter.declaration" | "v.receiver.declaration") => {
            Some(EntityKind::Parameter)
        }
        (SyntaxFactKind::Declaration, "v.type_parameter.declaration") => {
            Some(EntityKind::TypeParameter)
        }
        (SyntaxFactKind::Declaration, "v.import.declaration") => Some(EntityKind::Import),
        _ => None,
    }
}

fn identifier_start(character: char) -> bool {
    character.is_ascii_alphabetic() || matches!(character, '_' | 'α'..='ω' | 'Α'..='Ω' | 'µ')
}

pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    if text.len() > maximum_bytes {
        return None;
    }
    // Preserve the grammar's ordered optional prefixes without canonicalizing them.
    let mut identifier = text;
    for prefix in ["@", "$", "C.", "JS."] {
        identifier = identifier.strip_prefix(prefix).unwrap_or(identifier);
    }
    let mut characters = identifier.chars();
    (characters.next().is_some_and(identifier_start)
        && characters.all(|character| identifier_start(character) || character.is_ascii_digit()))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    let operator_role = matches!(
        (fact.kind(), fact.syntax_kind().as_str()),
        (
            SyntaxFactKind::Declaration,
            "v.function.declaration" | "v.method.declaration"
        ) | (
            SyntaxFactKind::Occurrence,
            "v.name.definition" | "v.written_call.call"
        )
    );
    if operator_role
        && text.len() <= maximum_bytes
        && matches!(
            text,
            "+" | "-" | "*" | "/" | "%" | "<" | ">" | "==" | "!=" | "<=" | ">="
        )
    {
        return Some(text);
    }
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "v.name.definition"
            | "v.identifier.reference"
            | "v.type.reference"
            | "v.written_call.call",
        )
        | (SyntaxFactKind::Declaration, "v.function.declaration" | "v.method.declaration")
        | (SyntaxFactKind::Import, "v.written_import.import") => captured_name(text, maximum_bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([6; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn v_declarations_require_exact_written_roles() {
        for (label, role, expected) in [
            ("v.file.module", SyntaxFactKind::Module, EntityKind::Module),
            (
                "v.module.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Module,
            ),
            (
                "v.struct.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Struct,
            ),
            (
                "v.union.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Union,
            ),
            (
                "v.interface.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Interface,
            ),
            (
                "v.enum.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Enum,
            ),
            (
                "v.type.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::TypeAlias,
            ),
            (
                "v.function.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Function,
            ),
            (
                "v.method.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Method,
            ),
            (
                "v.field.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Field,
            ),
            (
                "v.constant.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Constant,
            ),
            (
                "v.enumerator.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Constant,
            ),
            (
                "v.variable.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Variable,
            ),
            (
                "v.parameter.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Parameter,
            ),
            (
                "v.receiver.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Parameter,
            ),
            (
                "v.type_parameter.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::TypeParameter,
            ),
            (
                "v.import.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Import,
            ),
        ] {
            assert_eq!(
                crate::structural_entity_kind(&fact(role, label)),
                Some(expected)
            );
            assert_eq!(
                crate::structural_entity_kind(&fact(role, &format!("{label}.extra"))),
                None
            );
            assert_eq!(
                crate::structural_entity_kind(&fact(SyntaxFactKind::Occurrence, label)),
                None
            );
        }
        assert_eq!(
            crate::structural_entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "v.unreviewed_function.declaration"
            )),
            None
        );
        assert_eq!(
            crate::structural_entity_kind(&fact(SyntaxFactKind::Import, "v.written_import.import")),
            None
        );
        assert_eq!(
            crate::structural_captured_name_for_fact(
                "v",
                &fact(SyntaxFactKind::Import, "v.written_import.import"),
                "json",
                4,
            )
            .as_deref(),
            Some("json")
        );
    }

    #[test]
    fn v_identifier_spelling_keeps_prefixes_and_exact_grammar_alphabet() {
        for name in [
            "name",
            "Name_2",
            "_",
            "αωΑΩµ0",
            "@name",
            "$name",
            "C.name",
            "JS.name",
            "@$C.JS.Name",
        ] {
            assert_eq!(
                crate::structural_captured_name_for_language("v", name, name.len()).as_deref(),
                Some(name)
            );
            assert!(
                crate::structural_captured_name_for_language("v", name, name.len() - 1).is_none()
            );
        }
        for name in [
            "",
            "2name",
            "@",
            "$",
            "C.",
            "JS.",
            "$@name",
            "JS.C.name",
            "@@name",
            "x.y",
            "é",
            "Ж",
            "α١",
            "a\u{301}",
            "name value",
            "+",
            "==",
        ] {
            assert!(
                crate::structural_captured_name_for_language("v", name, usize::MAX).is_none(),
                "accepted {name:?}"
            );
        }
    }

    #[test]
    fn v_operator_names_require_exact_fact_and_language_proof() {
        for operator in ["+", "-", "*", "/", "%", "<", ">", "==", "!=", "<=", ">="] {
            for (role, label) in [
                (SyntaxFactKind::Declaration, "v.function.declaration"),
                (SyntaxFactKind::Declaration, "v.method.declaration"),
                (SyntaxFactKind::Occurrence, "v.name.definition"),
                (SyntaxFactKind::Occurrence, "v.written_call.call"),
            ] {
                let proof = fact(role, label);
                assert_eq!(
                    crate::structural_captured_name_for_fact("v", &proof, operator, operator.len())
                        .as_deref(),
                    Some(operator)
                );
                assert!(
                    crate::structural_captured_name_for_fact(
                        "v",
                        &proof,
                        operator,
                        operator.len() - 1
                    )
                    .is_none()
                );
                assert!(
                    crate::structural_captured_name_for_fact("rust", &proof, operator, usize::MAX)
                        .is_none()
                );
            }
            for label in [
                "v.identifier.reference",
                "v.type.reference",
                "v.unknown.call",
            ] {
                assert!(
                    crate::structural_captured_name_for_fact(
                        "v",
                        &fact(SyntaxFactKind::Occurrence, label),
                        operator,
                        usize::MAX
                    )
                    .is_none()
                );
            }
        }
        for text in ["<<", "&&", "=", "++", " +", "+ ", "∋"] {
            assert!(
                crate::structural_captured_name_for_fact(
                    "v",
                    &fact(SyntaxFactKind::Occurrence, "v.written_call.call"),
                    text,
                    usize::MAX
                )
                .is_none()
            );
        }
        for (language, label) in [("v", "rust.name.definition"), ("rust", "v.name.definition")] {
            assert!(
                crate::structural_captured_name_for_fact(
                    language,
                    &fact(SyntaxFactKind::Occurrence, label),
                    "name",
                    usize::MAX
                )
                .is_none()
            );
        }
    }
}
