//! Scheme facts retain reviewed written owners and raw symbol spelling.
//! Native callers prove complete default-reader symbols and executable contexts;
//! lexical admission never establishes reader, macro or runtime binding identity.

use std::str::Chars;

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

/// Classifies only exact physical file and written declaration labels.
///
/// Macro owners use the callable physical kind, not expanded declarations. Record
/// types and fields describe written structure, not implicitly generated bindings.
pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "scheme.file.module"
    {
        return Some(EntityKind::Module);
    }
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "scheme.procedure.declaration" | "scheme.macro.declaration" => Some(EntityKind::Function),
        "scheme.variable.declaration" => Some(EntityKind::Variable),
        "scheme.parameter.declaration" => Some(EntityKind::Parameter),
        "scheme.record_type.declaration" => Some(EntityKind::Struct),
        "scheme.field.declaration" => Some(EntityKind::Field),
        _ => None,
    }
}

/// Borrows one bounded written symbol without trimming, decoding or case folding.
///
/// The caller proves a clean complete native default-reader symbol and its context.
/// ASCII peculiar identifiers and escape boundaries follow the reviewed R5/R6/R7
/// union; full Unicode category membership remains native evidence. Admission does
/// not apply reader directives, equate spellings or resolve declaration ownership.
pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum_bytes || text.contains('\0') {
        return None;
    }
    let mut characters = text.chars();
    let first = characters.next()?;
    let accepted = match first {
        '|' => bar_symbol(&mut characters),
        '+' | '-' => match characters.next() {
            None => true,
            Some('>') if first == '-' => bare_tail(&mut characters),
            Some('.') => {
                characters.next().is_some_and(dot_subsequent) && characters.all(ascii_subsequent)
            }
            Some(next) => sign_subsequent(next) && characters.all(ascii_subsequent),
        },
        '.' => characters.next().is_some_and(dot_subsequent) && characters.all(ascii_subsequent),
        '\\' => {
            characters.next() == Some('x')
                && hex_escape_tail(&mut characters)
                && bare_tail(&mut characters)
        }
        _ => bare_initial(first) && bare_tail(&mut characters),
    };
    accepted.then_some(text)
}

/// Borrows only exact Scheme definition or unresolved symbol-reference occurrences.
///
/// The language dispatch owns the namespace fence; native capture, source spans
/// and generation provenance remain caller responsibilities.
pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    if fact.kind() != SyntaxFactKind::Occurrence {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "scheme.name.definition" | "scheme.symbol.reference" => captured_name(text, maximum_bytes),
        _ => None,
    }
}

fn ascii_initial(character: char) -> bool {
    character.is_ascii_alphabetic()
        || matches!(
            character,
            '!' | '$' | '%' | '&' | '*' | '/' | ':' | '<' | '=' | '>' | '?' | '^' | '_' | '~'
        )
}

fn ascii_subsequent(character: char) -> bool {
    ascii_initial(character)
        || character.is_ascii_digit()
        || matches!(character, '+' | '.' | '@' | '-')
}

fn sign_subsequent(character: char) -> bool {
    ascii_initial(character) || matches!(character, '+' | '-' | '@')
}

fn dot_subsequent(character: char) -> bool {
    sign_subsequent(character) || character == '.'
}

fn bare_initial(character: char) -> bool {
    ascii_initial(character)
        || !character.is_ascii() && !character.is_whitespace() && !character.is_control()
}

fn bare_tail(characters: &mut Chars<'_>) -> bool {
    while let Some(character) = characters.next() {
        if character == '\\' {
            if characters.next() != Some('x') || !hex_escape_tail(characters) {
                return false;
            }
        } else if !bare_initial(character)
            && !character.is_ascii_digit()
            && !matches!(character, '+' | '.' | '@' | '-')
        {
            return false;
        }
    }
    true
}

fn hex_escape_tail(characters: &mut Chars<'_>) -> bool {
    let mut has_digit = false;
    for character in characters.by_ref() {
        if character == ';' {
            return has_digit;
        }
        if !character.is_ascii_hexdigit() {
            return false;
        }
        has_digit = true;
    }
    false
}

fn bar_symbol(characters: &mut Chars<'_>) -> bool {
    while let Some(character) = characters.next() {
        match character {
            '|' => return characters.next().is_none(),
            '\\' => match characters.next() {
                Some('x' | 'X') if hex_escape_tail(characters) => {}
                Some('a' | 'b' | 't' | 'n' | 'r' | '|') => {}
                _ => return false,
            },
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    use super::*;
    use crate::{
        SyntaxKindLabel, structural_captured_name_for_fact, structural_captured_name_for_language,
        structural_entity_kind, structural_entity_kind_from_source,
    };

    const FACT_KINDS: [SyntaxFactKind; 11] = [
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

    const DECLARATIONS: [(&str, EntityKind); 6] = [
        ("scheme.procedure.declaration", EntityKind::Function),
        ("scheme.macro.declaration", EntityKind::Function),
        ("scheme.variable.declaration", EntityKind::Variable),
        ("scheme.parameter.declaration", EntityKind::Parameter),
        ("scheme.record_type.declaration", EntityKind::Struct),
        ("scheme.field.declaration", EntityKind::Field),
    ];

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
    fn reviewed_written_owners_require_exact_roles_and_labels() {
        for (label, expected) in DECLARATIONS {
            for kind in FACT_KINDS {
                let expected = (kind == SyntaxFactKind::Declaration).then_some(expected);
                let owner = fact(kind, label);
                assert_eq!(entity_kind(&owner), expected, "{kind:?}: {label}");
                assert_eq!(structural_entity_kind(&owner), expected);
                assert_eq!(
                    structural_entity_kind_from_source(&owner, "written owner"),
                    expected
                );
            }
        }
        for kind in FACT_KINDS {
            assert_eq!(
                structural_entity_kind(&fact(kind, "scheme.file.module")),
                (kind == SyntaxFactKind::Module).then_some(EntityKind::Module)
            );
        }
    }

    #[test]
    fn unknown_scheme_owners_never_acquire_generic_kinds() {
        for label in [
            "scheme.function.declaration",
            "scheme.unknown_function.declaration",
            "scheme.library.declaration",
            "scheme.record_constructor.declaration",
            "scheme.record_type.declaration.extra",
            "scheme.parameter.definition",
            "scheme.name.definition",
            "scheme.file.module.extra",
        ] {
            for kind in FACT_KINDS {
                assert_eq!(
                    structural_entity_kind(&fact(kind, label)),
                    None,
                    "{kind:?}: {label}"
                );
            }
        }
        for label in [
            "rust.procedure.declaration",
            "scheme_extra.procedure.declaration",
            "Scheme.procedure.declaration",
        ] {
            assert_eq!(entity_kind(&fact(SyntaxFactKind::Declaration, label)), None);
        }
    }

    #[test]
    fn written_names_borrow_exact_unicode_bars_and_escape_bytes() {
        for source in [
            "Mixed",
            "mixed",
            "λ",
            "雪",
            "nameλ",
            "a\u{301}",
            "name?",
            "!$%&*/:<=>?^_~",
            "+",
            "-",
            "...",
            "..",
            ".name",
            "-.name",
            "+@name",
            "->λ",
            r"\x61;Name",
            r"name\x3bb;",
            "||",
            "|Mixed Name|",
            "|line\nname|",
            r"|pipe\|name|",
            r"|\a\b\t\n\r|",
            r"|\x3bb;\X41;|",
            "|a(b);'`,\"#[]{}|",
        ] {
            let name = captured_name(source, source.len()).unwrap();
            assert_eq!(name, source, "{source:?}");
            assert!(std::ptr::eq(name.as_ptr(), source.as_ptr()));
            assert_eq!(captured_name(source, usize::MAX), Some(source));
            assert_eq!(captured_name(source, source.len() - 1), None);
            assert_eq!(captured_name(source, 0), None);
        }
        assert_eq!(captured_name("λ", 1), None);
        assert_eq!(captured_name("λ", 2), Some("λ"));
    }

    #[test]
    fn malformed_reader_shapes_do_not_trim_or_extract_names() {
        for source in [
            "",
            " name",
            "name ",
            "two names",
            "name\t",
            "name\n",
            "name\u{85}",
            "name\0tail",
            "|name\0tail|",
            "9name",
            ".",
            "+1",
            "-1",
            "+.",
            "+.1",
            "+λ",
            "(name)",
            "[name]",
            "{name}",
            "'name",
            "`name",
            ",name",
            "name;comment",
            "name\"tail",
            "#:name",
            "name#tail",
            "|name",
            "name|",
            "|name|tail",
            "a|fragment|",
            r"name\",
            r"\name",
            r"escaped\ name",
            r"\X61;",
            r"\x;",
            r"\x61",
            r"\x61g;",
            r"|\q|",
            r"|\x;|",
            r"|\x61|",
            r"|\\|",
            r"|name\|",
        ] {
            assert_eq!(captured_name(source, usize::MAX), None, "{source:?}");
        }
        assert_eq!(captured_name("", 0), None);
    }

    #[test]
    fn written_spellings_are_not_reader_equivalence() {
        for (left, right) in [
            ("Mixed", "mixed"),
            ("name", "|name|"),
            ("name", r"\x6e;ame"),
            ("λ", r"\x3bb;"),
            ("|λ|", r"|\x3bb;|"),
            ("ns:name", "name"),
        ] {
            assert_ne!(
                captured_name(left, 64).unwrap(),
                captured_name(right, 64).unwrap()
            );
        }
    }

    #[test]
    fn only_exact_definition_and_symbol_reference_occurrences_have_names() {
        for label in ["scheme.name.definition", "scheme.symbol.reference"] {
            for kind in FACT_KINDS {
                let occurrence = fact(kind, label);
                let expected = (kind == SyntaxFactKind::Occurrence).then_some("|Written Name|");
                assert_eq!(
                    captured_name_for_fact(&occurrence, "|Written Name|", 64),
                    expected
                );
                assert_eq!(
                    structural_captured_name_for_fact("scheme", &occurrence, "|Written Name|", 64),
                    expected.map(Cow::Borrowed)
                );
            }
        }
        for label in [
            "scheme.name.reference",
            "scheme.symbol.definition",
            "scheme.name.definition.extra",
            "scheme.call.reference",
            "scheme.procedure.declaration",
            "scheme.unknown.reference",
            "rust.name.definition",
            "scheme_extra.name.definition",
            "Scheme.name.definition",
        ] {
            assert_eq!(
                structural_captured_name_for_fact(
                    "scheme",
                    &fact(SyntaxFactKind::Occurrence, label),
                    "Written",
                    64
                ),
                None,
                "{label}"
            );
        }
    }

    #[test]
    fn public_name_dispatch_fences_foreign_languages_and_invalid_spelling() {
        for label in ["scheme.name.definition", "scheme.symbol.reference"] {
            let occurrence = fact(SyntaxFactKind::Occurrence, label);
            for foreign in [
                "rust",
                "common-lisp",
                "lisp",
                "racket",
                "Scheme",
                "SCHEME",
                "",
            ] {
                assert_eq!(
                    structural_captured_name_for_fact(foreign, &occurrence, "Written", 64),
                    None,
                    "{foreign}"
                );
            }
            for invalid in [r"escaped\ name", "name|tail|", "(name)", ""] {
                assert_eq!(
                    structural_captured_name_for_fact("scheme", &occurrence, invalid, 64),
                    None
                );
            }
            assert_eq!(
                structural_captured_name_for_fact("scheme", &occurrence, "λ", 1),
                None
            );
            assert_eq!(
                structural_captured_name_for_fact("scheme", &occurrence, "λ", 2),
                Some(Cow::Borrowed("λ"))
            );
        }
        assert_eq!(
            structural_captured_name_for_language("scheme", "|Written Name|", 14),
            Some(Cow::Borrowed("|Written Name|"))
        );
        assert_eq!(
            structural_captured_name_for_language("scheme", "|Written Name|", 13),
            None
        );
        assert_eq!(
            structural_captured_name_for_language("scheme", r"escaped\ name", 64),
            None
        );
    }
}
