//! Exact Q# native labels and written identifier spelling.
use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;
pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "qsharp.namespace.declaration" => Some(EntityKind::Namespace),
        "qsharp.function.declaration" => Some(EntityKind::Function),
        "qsharp.parameter.declaration" => Some(EntityKind::Parameter),
        _ => None,
    }
}
pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum {
        return None;
    }
    text.split('.')
        .all(|part| {
            let Some((first, rest)) = part.as_bytes().split_first() else {
                return false;
            };
            (*first == b'_' || first.is_ascii_alphabetic())
                && rest.iter().all(|b| *b == b'_' || b.is_ascii_alphanumeric())
                && !matches!(
                    part,
                    "_" | "namespace"
                        | "operation"
                        | "function"
                        | "internal"
                        | "let"
                        | "mutable"
                        | "set"
                        | "use"
                        | "borrow"
                        | "return"
                        | "body"
                        | "adjoint"
                        | "controlled"
                        | "auto"
                        | "intrinsic"
                        | "open"
                        | "import"
                        | "newtype"
                        | "struct"
                        | "if"
                        | "else"
                        | "for"
                        | "while"
                        | "repeat"
                        | "until"
                        | "within"
                        | "apply"
                        | "fail"
                        | "true"
                        | "false"
                )
        })
        .then_some(text)
}
pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    let label = fact.syntax_kind().as_str();
    if !matches!(
        (fact.kind(), label),
        (
            SyntaxFactKind::Occurrence,
            "qsharp.name.definition"
                | "qsharp.formal_argument.reference"
                | "qsharp.unresolved_name.reference"
        )
    ) {
        return None;
    }
    captured_name(text, maximum)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespace_paths_keep_case_while_selectors_calls_and_omitted_binders_do_not() {
        for name in ["Written.Space", "Measure", "value"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None)
        }
        for name in [
            "",
            "_",
            "Written..Space",
            "value()",
            "value[0]",
            "operation",
            "λ",
        ] {
            assert_eq!(captured_name(name, 64), None)
        }
    }
    #[test]
    fn exact_native_declaration_labels_do_not_promote_unknown_or_foreign_kinds() {
        for (label, kind) in [
            ("qsharp.namespace.declaration", Some(EntityKind::Namespace)),
            ("qsharp.function.declaration", Some(EntityKind::Function)),
            ("qsharp.parameter.declaration", Some(EntityKind::Parameter)),
            ("qsharp.unproven.declaration", None),
            ("python.function.declaration", None),
        ] {
            let f = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                rootlight_ir::SourceSpan::new(rootlight_ids::FileId::from_bytes([7; 20]), 0, 1)
                    .unwrap(),
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(entity_kind(&f), kind)
        }
    }
}
