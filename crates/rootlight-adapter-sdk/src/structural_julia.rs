//! Julia names retain the written native spelling and declaration category.
//! Operator and qualified names are source identities, without dispatch inference.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;
use std::borrow::Cow;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "julia.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "julia.module.declaration") => Some(EntityKind::Module),
        (
            SyntaxFactKind::Declaration,
            "julia.function.declaration"
            | "julia.macro.declaration"
            | "julia.callable_object.declaration"
            | "julia.anonymous_function.declaration",
        ) => Some(EntityKind::Function),
        (
            SyntaxFactKind::Declaration,
            "julia.struct.declaration" | "julia.primitive.declaration",
        ) => Some(EntityKind::Struct),
        (SyntaxFactKind::Declaration, "julia.abstract.declaration") => Some(EntityKind::Class),
        (
            SyntaxFactKind::Declaration,
            "julia.variable.declaration" | "julia.global_variable.declaration",
        ) => Some(EntityKind::Variable),
        (
            SyntaxFactKind::Declaration,
            "julia.parameter.declaration" | "julia.unnamed_parameter.declaration",
        ) => Some(EntityKind::Parameter),
        (SyntaxFactKind::Declaration, "julia.type_parameter.declaration") => {
            Some(EntityKind::TypeParameter)
        }
        (SyntaxFactKind::Declaration, "julia.field.declaration") => Some(EntityKind::Field),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    (!text.is_empty() && text.len() <= maximum_bytes && !text.chars().any(char::is_control))
        .then_some(text)
}

fn quoted_name(text: &str, maximum_bytes: usize) -> Option<Cow<'_, str>> {
    if text.starts_with("var\"\"\"") {
        return None;
    }
    let body = text.strip_prefix("var\"")?.strip_suffix('"')?;
    if !body.contains('\\') {
        return (body.len() <= maximum_bytes && !body.contains(['"', '\0']))
            .then_some(Cow::Borrowed(body));
    }
    let mut decoded = String::new();
    decoded
        .try_reserve_exact(body.len().min(maximum_bytes))
        .ok()?;
    let mut push = |character: char| {
        if character == '\0' || decoded.len().checked_add(character.len_utf8())? > maximum_bytes {
            return None;
        }
        decoded.push(character);
        Some(())
    };
    let mut characters = body.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '"' {
            return None;
        }
        if character != '\\' {
            push(character)?;
            continue;
        }
        let mut slashes = 1_usize;
        while characters.peek() == Some(&'\\') {
            characters.next();
            slashes = slashes.checked_add(1)?;
        }
        // Julia's parser uses raw escapes: only quotes and the closing
        // delimiter halve backslash runs. Other sequences remain literal.
        let quoted = characters.peek() == Some(&'"');
        let trailing = characters.peek().is_none();
        if quoted && slashes.is_multiple_of(2) || trailing && !slashes.is_multiple_of(2) {
            return None;
        }
        let retained = if quoted || trailing {
            slashes / 2
        } else {
            slashes
        };
        for _ in 0..retained {
            push('\\')?;
        }
        if quoted {
            characters.next();
            push('"')?;
        }
    }
    Some(Cow::Owned(decoded))
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<Cow<'a, str>> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "julia.name.definition" | "julia.written_call.call" | "julia.identifier.reference",
        )
        | (SyntaxFactKind::Import, "julia.written_import.import") => {
            if text.starts_with("var\"") {
                quoted_name(text, maximum_bytes)
            } else {
                captured_name(text, maximum_bytes).map(Cow::Borrowed)
            }
        }
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
            SourceSpan::new(FileId::from_bytes([4; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn julia_written_names_preserve_unicode_operators_and_qualified_spelling() {
        for name in ["λ!", "Base.take!", "⊗", "==", "Name", "name"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in ["", "line\nnext", "nul\0"] {
            assert_eq!(captured_name(name, usize::MAX), None);
        }
        for (label, kind) in [
            ("julia.module.declaration", EntityKind::Module),
            ("julia.function.declaration", EntityKind::Function),
            ("julia.anonymous_function.declaration", EntityKind::Function),
            ("julia.macro.declaration", EntityKind::Function),
            ("julia.struct.declaration", EntityKind::Struct),
            ("julia.abstract.declaration", EntityKind::Class),
            ("julia.primitive.declaration", EntityKind::Struct),
            ("julia.parameter.declaration", EntityKind::Parameter),
            ("julia.unnamed_parameter.declaration", EntityKind::Parameter),
            (
                "julia.type_parameter.declaration",
                EntityKind::TypeParameter,
            ),
            ("julia.field.declaration", EntityKind::Field),
            ("julia.variable.declaration", EntityKind::Variable),
            ("julia.global_variable.declaration", EntityKind::Variable),
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                Some(kind)
            );
            assert_eq!(entity_kind(&fact(SyntaxFactKind::Occurrence, label)), None);
        }
    }

    #[test]
    fn julia_quoted_identifiers_decode_raw_escapes_without_evaluation() {
        for label in [
            "julia.name.definition",
            "julia.written_call.call",
            "julia.identifier.reference",
        ] {
            let occurrence = fact(SyntaxFactKind::Occurrence, label);
            for (written, canonical) in [
                (r##"var"#internal#""##, "#internal#"),
                (r#"var"λ😀""#, "λ😀"),
                (r#"var"with space""#, "with space"),
                (r#"var"a\"b""#, "a\"b"),
                (r#"var"trailing\\""#, "trailing\\"),
                (r#"var"raw\x""#, "raw\\x"),
                (r#"var"$literal""#, "$literal"),
            ] {
                assert_eq!(
                    crate::structural_captured_name_for_fact(
                        "julia",
                        &occurrence,
                        written,
                        canonical.len()
                    )
                    .as_deref(),
                    Some(canonical),
                    "{written}"
                );
                assert!(
                    crate::structural_captured_name_for_fact(
                        "julia",
                        &occurrence,
                        written,
                        canonical.len() - 1
                    )
                    .is_none()
                );
            }
            for invalid in [
                r#"var"unterminated"#,
                r#"var"ok"suffix"#,
                r#"var"""triple""""#,
                r#"var"a"b""#,
                r#"var"trailing\""#,
                "var\"nul\0name\"",
            ] {
                assert!(
                    crate::structural_captured_name_for_fact(
                        "julia",
                        &occurrence,
                        invalid,
                        usize::MAX
                    )
                    .is_none(),
                    "{invalid}"
                );
            }
        }
    }

    #[test]
    fn julia_quoted_empty_and_control_names_keep_exact_decoded_identity() {
        for (kind, label) in [
            (SyntaxFactKind::Occurrence, "julia.name.definition"),
            (SyntaxFactKind::Occurrence, "julia.written_call.call"),
            (SyntaxFactKind::Occurrence, "julia.identifier.reference"),
            (SyntaxFactKind::Import, "julia.written_import.import"),
        ] {
            let capture = fact(kind, label);
            for (written, canonical) in [
                ("var\"\"", ""),
                ("var\"line\nname\"", "line\nname"),
                ("var\"tab\tname\"", "tab\tname"),
                ("var\"λ\n😀\"", "λ\n😀"),
                (r#"var"raw\n\0""#, r"raw\n\0"),
            ] {
                assert_eq!(
                    captured_name_for_fact(&capture, written, canonical.len()).as_deref(),
                    Some(canonical),
                    "{written:?}"
                );
                if let Some(smaller) = canonical.len().checked_sub(1) {
                    assert!(captured_name_for_fact(&capture, written, smaller).is_none());
                }
            }
            assert!(captured_name_for_fact(&capture, "var\"bad\0name\"", usize::MAX).is_none());
        }
        assert_eq!(captured_name("", 0), None);
        assert_eq!(captured_name("line\nname", 64), None);
        let foreign = fact(SyntaxFactKind::Occurrence, "rust.name.definition");
        assert!(captured_name_for_fact(&foreign, "var\"\"", 64).is_none());
    }

    #[test]
    fn julia_occurrence_names_require_matching_language_and_role() {
        let definition = fact(SyntaxFactKind::Occurrence, "julia.name.definition");
        assert_eq!(
            crate::structural_captured_name_for_fact("julia", &definition, "⊗", 3).as_deref(),
            Some("⊗")
        );
        assert!(crate::structural_captured_name_for_fact("rust", &definition, "⊗", 3).is_none());
        for label in [
            "julia.unknown.reference",
            "rust.name.definition",
            "julia.function.declaration",
        ] {
            assert_eq!(
                captured_name_for_fact(&fact(SyntaxFactKind::Occurrence, label), "name", 4),
                None
            );
        }
    }
}
