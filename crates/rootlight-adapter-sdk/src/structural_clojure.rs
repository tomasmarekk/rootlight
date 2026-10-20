//! Clojure identities are written names, distinct from reader/runtime bindings.
//! Native context owns complete symbol and declaration proof; spelling stays raw.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "clojure.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "clojure.namespace.declaration") => {
            Some(EntityKind::Namespace)
        }
        (SyntaxFactKind::Declaration, "clojure.function.declaration") => Some(EntityKind::Function),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut chars = text.chars();
    let first = chars.next()?;
    let forbidden = |c: char| {
        c.is_control()
            || c.is_whitespace()
            || matches!(
                c,
                '(' | ')' | '[' | ']' | '{' | '}' | '"' | '@' | '~' | '^' | ';' | '`' | '\\' | ','
            )
    };
    if text.len() > maximum
        || forbidden(first)
        || first.is_ascii_digit()
        || matches!(first, ':' | '#' | '\'')
        || chars.any(forbidden)
    {
        return None;
    }
    if text != "/"
        && (text.starts_with('/') || text.ends_with('/') || text.matches('/').count() > 1)
    {
        return None;
    }
    Some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "clojure.name.definition"
                | "clojure.call.reference"
                | "clojure.dynamic_call.reference"
                | "clojure.symbol.reference"
        ))
    .then(|| captured_name(text, maximum))
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;
    #[test]
    fn symbols_keep_case_punctuation_and_complete_byte_bounds() {
        for name in [
            "lt.util.kahn",
            "without",
            "take-1",
            "no-incoming",
            "g'",
            "λ",
            "set/union",
        ] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert!(captured_name(name, name.len() - 1).is_none());
        }
        assert_ne!(captured_name("Case", 4), captured_name("case", 4));
        for name in [
            "",
            "1name",
            ":keyword",
            "#dispatch",
            "'quote",
            "two names",
            "|two names|",
            "a\\b",
            "a/b/c",
            "a/",
            "x\0y",
        ] {
            assert!(captured_name(name, 64).is_none(), "{name:?}");
        }
    }

    #[test]
    fn only_exact_clojure_occurrence_labels_supply_written_names() {
        for kind in [
            SyntaxFactKind::Declaration,
            SyntaxFactKind::Occurrence,
            SyntaxFactKind::Comment,
        ] {
            for label in [
                "clojure.name.definition",
                "clojure.call.reference",
                "scheme.name.definition",
                "clojure.unavailable.expression",
            ] {
                let fact = SyntaxFact::new(
                    1,
                    None,
                    kind,
                    SourceSpan::new(FileId::from_bytes([1; 20]), 0, 1).unwrap(),
                    0,
                    SyntaxKindLabel::new(label).unwrap(),
                );
                let expected = (kind == SyntaxFactKind::Occurrence
                    && matches!(label, "clojure.name.definition" | "clojure.call.reference"))
                .then_some("written?");
                assert_eq!(captured_name_for_fact(&fact, "written?", 64), expected);
            }
        }
    }
}
