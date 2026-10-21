//! Exact native Chapel structural labels and ordinary written identifier spelling.
use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

const KEYWORDS: &[&str] = &[
    "_",
    "align",
    "as",
    "atomic",
    "begin",
    "bool",
    "borrowed",
    "break",
    "by",
    "bytes",
    "catch",
    "class",
    "cobegin",
    "coforall",
    "complex",
    "config",
    "const",
    "continue",
    "defer",
    "deinit",
    "delete",
    "dmapped",
    "do",
    "domain",
    "else",
    "enum",
    "except",
    "export",
    "extern",
    "false",
    "for",
    "forall",
    "foreach",
    "forwarding",
    "if",
    "imag",
    "implements",
    "in",
    "index",
    "init",
    "inline",
    "inout",
    "int",
    "interface",
    "iter",
    "label",
    "let",
    "lifetime",
    "local",
    "locale",
    "manage",
    "module",
    "new",
    "nil",
    "noinit",
    "on",
    "only",
    "operator",
    "otherwise",
    "out",
    "override",
    "owned",
    "param",
    "postinit",
    "private",
    "proc",
    "prototype",
    "public",
    "range",
    "real",
    "record",
    "reduce",
    "ref",
    "require",
    "return",
    "scan",
    "select",
    "serial",
    "shared",
    "single",
    "sparse",
    "string",
    "subdomain",
    "super",
    "sync",
    "then",
    "these",
    "this",
    "throw",
    "throws",
    "true",
    "try",
    "type",
    "uint",
    "union",
    "unmanaged",
    "use",
    "var",
    "void",
    "when",
    "where",
    "while",
    "with",
    "yield",
    "zip",
    "lambda",
    "pragma",
    "primitive",
];

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "chapel.module.declaration" => Some(EntityKind::Module),
        "chapel.function.declaration" => Some(EntityKind::Function),
        "chapel.parameter.declaration" => Some(EntityKind::Parameter),
        _ => None,
    }
}
pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum || KEYWORDS.contains(&text) {
        return None;
    }
    let (first, rest) = text.as_bytes().split_first()?;
    if !first.is_ascii_alphabetic() && *first != b'_' {
        return None;
    }
    rest.iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        .then_some(text)
}
pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    if fact.kind() == SyntaxFactKind::Declaration {
        entity_kind(fact)?;
    } else if fact.kind() != SyntaxFactKind::Occurrence
        || !matches!(
            fact.syntax_kind().as_str(),
            "chapel.name.definition"
                | "chapel.unresolved_name.reference"
                | "chapel.formal_initializer.reference"
                | "chapel.typed_local_call.reference"
        )
    {
        return None;
    }
    captured_name(text, maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            rootlight_ir::SourceSpan::new(rootlight_ids::FileId::from_bytes([7; 20]), 0, 1)
                .unwrap(),
            0,
            crate::SyntaxKindLabel::new(label).unwrap(),
        )
    }
    #[test]
    fn exact_declaration_labels_preserve_module_function_and_formal_kinds() {
        for (label, expected) in [
            ("chapel.module.declaration", Some(EntityKind::Module)),
            ("chapel.function.declaration", Some(EntityKind::Function)),
            ("chapel.parameter.declaration", Some(EntityKind::Parameter)),
            ("chapel.call.declaration", None),
            ("chapel.name.definition", None),
            ("rust.function.declaration", None),
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                expected
            );
            assert_eq!(entity_kind(&fact(SyntaxFactKind::Signature, label)), None);
        }
    }
    #[test]
    fn written_names_reject_keyword_selector_foreign_label_and_short_byte_budget() {
        assert_eq!(captured_name("Written_2$", 10), Some("Written_2$"));
        for text in [
            "proc",
            "begin",
            "_",
            "module",
            "Written.value",
            "value()",
            "1value",
            "λ",
        ] {
            assert_eq!(captured_name(text, 128), None);
        }
        let declaration = fact(SyntaxFactKind::Declaration, "chapel.parameter.declaration");
        assert_eq!(
            captured_name_for_fact(&declaration, "value", 5),
            Some("value")
        );
        assert_eq!(captured_name_for_fact(&declaration, "value", 4), None);
        assert_eq!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "python.identifier.reference"),
                "value",
                5
            ),
            None
        );
    }
}
#[cfg(test)]
mod initializer_read_tests {
    use super::*;

    #[test]
    fn initializer_label_borrows_only_the_exact_plain_occurrence_name() {
        let span = rootlight_ir::SourceSpan::new(rootlight_ids::FileId::from_bytes([7; 20]), 0, 5)
            .unwrap();
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                span,
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            )
        };
        let read = fact(
            SyntaxFactKind::Occurrence,
            "chapel.formal_initializer.reference",
        );
        assert_eq!(captured_name_for_fact(&read, "value", 5), Some("value"));
        assert_eq!(captured_name_for_fact(&read, "value", 4), None);
        for value in ["proc", "_", "object.value", "value()", "λ"] {
            assert_eq!(captured_name_for_fact(&read, value, 128), None);
        }
        for (kind, label) in [
            (
                SyntaxFactKind::Declaration,
                "chapel.formal_initializer.reference",
            ),
            (
                SyntaxFactKind::Signature,
                "chapel.formal_initializer.reference",
            ),
            (
                SyntaxFactKind::Occurrence,
                "chapel.formal_initializer_context.signature",
            ),
            (SyntaxFactKind::Occurrence, "rust.identifier.reference"),
        ] {
            assert_eq!(captured_name_for_fact(&fact(kind, label), "value", 5), None);
        }
    }
}

#[cfg(test)]
mod typed_call_tests {
    use super::*;

    #[test]
    fn typed_call_name_requires_the_exact_occurrence_label_and_byte_limit() {
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                rootlight_ir::SourceSpan::new(rootlight_ids::FileId::from_bytes([7; 20]), 0, 5)
                    .unwrap(),
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            )
        };
        let site = fact(
            SyntaxFactKind::Occurrence,
            "chapel.typed_local_call.reference",
        );
        assert_eq!(captured_name_for_fact(&site, "route", 5), Some("route"));
        assert_eq!(captured_name_for_fact(&site, "route", 4), None);
        for value in ["proc", "_", "obj.route", "route()", "λ"] {
            assert_eq!(captured_name_for_fact(&site, value, 128), None);
        }
        for (kind, label) in [
            (
                SyntaxFactKind::Declaration,
                "chapel.typed_local_call.reference",
            ),
            (
                SyntaxFactKind::Signature,
                "chapel.typed_local_call.reference",
            ),
            (
                SyntaxFactKind::Occurrence,
                "chapel.typed_local_call.reference.extra",
            ),
            (
                SyntaxFactKind::Occurrence,
                "chapel.typed_local_call_context.signature",
            ),
            (
                SyntaxFactKind::Occurrence,
                "rust.typed_local_call.reference",
            ),
        ] {
            assert_eq!(captured_name_for_fact(&fact(kind, label), "route", 5), None);
        }
    }
}
