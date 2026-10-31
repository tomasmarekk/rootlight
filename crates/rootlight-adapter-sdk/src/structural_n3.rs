//! Admit only exact source names certified by the native N3 capture path.
//! Relative IRIs remain written names, without base expansion or resource equivalence.

use super::*;

pub(super) fn name<'a>(fact: &SyntaxFact, text: &'a str, maximum: usize) -> Option<Cow<'a, str>> {
    if fact.kind() != SyntaxFactKind::Occurrence
        || text.is_empty()
        || text.len() > maximum
        || text.chars().any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return None;
    }
    let valid = match fact.syntax_kind().as_str() {
        "n3.prefix.definition" => !text.contains([
            ':', '<', '>', '\\', '"', '\'', '/', '?', '#', '@', '[', ']', '(', ')', '{', '}', ';',
            ',',
        ]),
        "n3.universal_variable.definition" | "n3.existential_variable.definition" => {
            if let Some(iri) = text
                .strip_prefix('<')
                .and_then(|text| text.strip_suffix('>'))
            {
                escaped_term(iri, true)
            } else {
                text.contains(':') && escaped_term(text, false)
            }
        }
        _ => false,
    };
    valid.then_some(Cow::Borrowed(text))
}

fn escaped_term(text: &str, iri: bool) -> bool {
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let Some(escape) = chars.next() else {
                return false;
            };
            if iri {
                let digits = match escape {
                    'u' => 4,
                    'U' => 8,
                    _ => return false,
                };
                if !(0..digits).all(|_| chars.next().is_some_and(|ch| ch.is_ascii_hexdigit())) {
                    return false;
                }
            } else if !"_~.-!$&'()*+,;=/?#@%".contains(escape) {
                return false;
            }
        } else if ch.is_control()
            || ch.is_whitespace()
            || if iri {
                "<>\"{}|^`".contains(ch)
            } else {
                "<>\"'/?#@[](){};,=^`|".contains(ch)
            }
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn escaped_terms_preserve_only_native_uchar_and_local_escape_forms() {
        for (text, iri, expected) in [
            (r"urn:\u0058", true, true),
            (r"urn:\U00000058", true, true),
            (r"urn:\u005", true, false),
            (r"urn:\u00GG", true, false),
            (r"urn:\#", true, false),
            (r"ex:has\#tag", false, true),
            (r"ex:has#tag", false, false),
            (r"ex:has\ntag", false, false),
            ("ex:trailing\\", false, false),
        ] {
            assert_eq!(escaped_term(text, iri), expected, "{text:?}");
        }
    }

    #[test]
    fn n3_names_require_exact_language_role_label_and_whole_bounded_term() {
        for (language, role, label, text, maximum, expected) in [
            (
                "n3",
                SyntaxFactKind::Occurrence,
                "n3.prefix.definition",
                "var",
                3,
                true,
            ),
            (
                "n3",
                SyntaxFactKind::Occurrence,
                "n3.universal_variable.definition",
                "<#X>",
                4,
                true,
            ),
            (
                "n3",
                SyntaxFactKind::Occurrence,
                "n3.existential_variable.definition",
                "var:x_0",
                7,
                true,
            ),
            (
                "turtle",
                SyntaxFactKind::Occurrence,
                "n3.universal_variable.definition",
                "<#X>",
                4,
                false,
            ),
            (
                "n3",
                SyntaxFactKind::Signature,
                "n3.universal_variable.definition",
                "<#X>",
                4,
                false,
            ),
            (
                "n3",
                SyntaxFactKind::Occurrence,
                "n3.universal_variable.reference",
                "<#X>",
                4,
                false,
            ),
            (
                "n3",
                SyntaxFactKind::Occurrence,
                "n3.universal_variable.definition",
                "<#X>",
                3,
                false,
            ),
            (
                "n3",
                SyntaxFactKind::Occurrence,
                "n3.universal_variable.definition",
                "?x",
                32,
                false,
            ),
            (
                "n3",
                SyntaxFactKind::Occurrence,
                "n3.universal_variable.definition",
                "\"x\"",
                32,
                false,
            ),
            (
                "n3",
                SyntaxFactKind::Occurrence,
                "n3.existential_variable.definition",
                "var:x, var:y",
                32,
                false,
            ),
            (
                "n3",
                SyntaxFactKind::Occurrence,
                "n3.prefix.definition",
                "",
                32,
                false,
            ),
            (
                "n3",
                SyntaxFactKind::Occurrence,
                "n3.prefix.definition",
                "var:",
                32,
                false,
            ),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                role,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 4).unwrap(),
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                super::super::structural_captured_name_for_fact(language, &fact, text, maximum)
                    .is_some(),
                expected,
                "{language} {label} {text:?}"
            );
        }
    }
}
