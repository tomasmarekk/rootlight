//! Bounded TOML key-path identities, independent of table declaration order.
//! Each decoded segment stays quoted so dots, empty keys and literal escape
//! text cannot collapse into the same identity as another path.

use std::{borrow::Cow, iter::Peekable, str::Chars};

use crate::json_names::{append, append_character};

pub(crate) fn canonical_toml_key_path(text: &str, maximum_bytes: usize) -> Option<Cow<'_, str>> {
    if text.len() > maximum_bytes {
        return None;
    }
    let mut characters = text.chars().peekable();
    let mut output = String::new();
    loop {
        skip_space(&mut characters);
        append(&mut output, "\"", maximum_bytes)?;
        match characters.peek().copied()? {
            quote @ ('\'' | '"') => {
                characters.next();
                loop {
                    let character = characters.next()?;
                    if character == quote {
                        break;
                    }
                    let decoded = if quote == '"' && character == '\\' {
                        escape(&mut characters)?
                    } else {
                        if matches!(character, '\0'..='\u{8}' | '\n'..='\u{1f}' | '\u{7f}') {
                            return None;
                        }
                        character
                    };
                    append_character(&mut output, decoded, maximum_bytes)?;
                }
            }
            character if bare(character) => {
                while let Some(character) = characters.next_if(|character| bare(*character)) {
                    append_character(&mut output, character, maximum_bytes)?;
                }
            }
            _ => return None,
        }
        append(&mut output, "\"", maximum_bytes)?;
        skip_space(&mut characters);
        match characters.next() {
            None => break,
            Some('.') => append(&mut output, ".", maximum_bytes)?,
            Some(_) => return None,
        }
    }
    if output == text {
        Some(Cow::Borrowed(text))
    } else {
        Some(Cow::Owned(output))
    }
}

fn skip_space(characters: &mut Peekable<Chars<'_>>) {
    while characters
        .next_if(|character| matches!(character, ' ' | '\t'))
        .is_some()
    {}
}

fn bare(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}

fn escape(characters: &mut Peekable<Chars<'_>>) -> Option<char> {
    match characters.next()? {
        'b' => Some('\u{8}'),
        't' => Some('\t'),
        'n' => Some('\n'),
        'f' => Some('\u{c}'),
        'r' => Some('\r'),
        'e' => Some('\u{1b}'),
        '"' => Some('"'),
        '\\' => Some('\\'),
        marker @ ('x' | 'u' | 'U') => {
            let digits = match marker {
                'x' => 2,
                'u' => 4,
                _ => 8,
            };
            let mut scalar = 0u32;
            for _ in 0..digits {
                let digit = characters.next()?.to_digit(16)?;
                scalar = scalar.checked_mul(16)?.checked_add(digit)?;
            }
            // TOML encodes Unicode scalars, not JSON's potentially unpaired
            // UTF-16 units. Even a pair of surrogate escapes is invalid here.
            char::from_u32(scalar)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_decodes_one_segment_but_preserves_dotted_path_boundaries() {
        for (source, expected) in [
            ("a", "a"),
            ("'a.b'", "a.b"),
            ("a.b", "\"a\".\"b\""),
            ("''", "\"\""),
            ("' '", "\" \""),
            (r#""\u0000""#, r#""\u0000""#),
            (r#"'a"b'"#, "a\"b"),
            (r#"'a"b'.c"#, r#""a\"b"."c""#),
        ] {
            let canonical = canonical_toml_key_path(source, 128).unwrap();
            assert_eq!(
                crate::structural_display_name_for_language("toml", &canonical),
                expected
            );
        }
    }

    #[test]
    fn equivalent_segments_share_identity_without_losing_path_boundaries() {
        for (source, expected) in [
            ("a", r#""a""#),
            ("'a'", r#""a""#),
            (r#""\x61""#, r#""a""#),
            (r#""\u0061""#, r#""a""#),
            (r#""\U00000061""#, r#""a""#),
            (" a . 'b' \t", r#""a"."b""#),
            ("3.14159", r#""3"."14159""#),
            ("''", r#""""#),
            ("' ' . ''", r#"" "."""#),
            (r#"'a.b'."c.d""#, r#""a.b"."c.d""#),
            (r#""\e""#, r#""\u001b""#),
            ("'\t'", r#""\u0009""#),
            (r#"'\u0061'"#, r#""\\u0061""#),
            (r#""\U0001F30D""#, r#""🌍""#),
            (r#"'a"b'"#, r#""a\"b""#),
        ] {
            let result = canonical_toml_key_path(source, 128).expect("valid key path");
            assert_eq!(result, expected, "{source}");
            assert_eq!(
                canonical_toml_key_path(&result, 128).as_deref(),
                Some(expected)
            );
            assert!(matches!(
                canonical_toml_key_path(expected, 128),
                Some(Cow::Borrowed(_))
            ));
        }
    }

    #[test]
    fn distinct_paths_do_not_collapse_or_normalize_unicode() {
        for (left, right) in [
            ("a.b", "'a.b'"),
            ("'a'.'b.c'", "'a.b'.'c'"),
            ("''", "' '"),
            ("''.''", "'.'"),
            (r#""\u0000""#, r#"'\u0000'"#),
            (r#""é""#, r#""e\u0301""#),
            ("A", "a"),
            (r#"'a"."b'"#, "a.b"),
        ] {
            let left = canonical_toml_key_path(left, 128).unwrap();
            let right = canonical_toml_key_path(right, 128).unwrap();
            assert_ne!(left, right);
        }
    }

    #[test]
    fn malformed_paths_and_non_scalar_escapes_are_rejected() {
        for source in [
            "",
            " ",
            ".",
            "a.",
            ".a",
            "a..b",
            "a b",
            "a\nb",
            "a.\nb",
            "a # comment",
            "a\u{a0}.b",
            "é",
            "'a",
            "\"a",
            "'''a'''",
            "\"\"\"a\"\"\"",
            "[a]",
            "a=1",
            "'a'b",
            r#""\v""#,
            r#""\/""#,
            r#""\xF""#,
            r#""\u123G""#,
            r#""\uD800""#,
            r#""\uD83C\uDF0D""#,
            r#""\U00110000""#,
            r#""\UFFFFFFFF""#,
        ] {
            assert!(canonical_toml_key_path(source, 128).is_none(), "{source:?}");
        }
        for control in (0u8..=31).chain([127]).filter(|value| *value != b'\t') {
            for quote in ['\'', '"'] {
                let source = format!("{quote}{}{quote}", char::from(control));
                assert!(
                    canonical_toml_key_path(&source, 128).is_none(),
                    "{source:?}"
                );
            }
        }
    }

    #[test]
    fn input_and_expanded_output_each_obey_the_byte_budget() {
        for (source, expected) in [
            ("a.b", r#""a"."b""#),
            ("'\t'", r#""\u0009""#),
            ("'🌍'", r#""🌍""#),
        ] {
            assert_eq!(
                canonical_toml_key_path(source, expected.len()).as_deref(),
                Some(expected)
            );
            assert!(canonical_toml_key_path(source, expected.len() - 1).is_none());
        }
        let source = r#""\U00000061""#;
        assert!(canonical_toml_key_path(source, source.len() - 1).is_none());
        assert_eq!(
            canonical_toml_key_path(source, source.len()).as_deref(),
            Some(r#""a""#)
        );
        assert!(canonical_toml_key_path("''", 1).is_none());
    }

    #[test]
    fn every_bmp_scalar_and_supplementary_boundaries_have_idempotent_identity() {
        for scalar in (0u32..=0xffff).chain([0x10000, 0x1f30d, 0x10ffff]) {
            let source = format!("\"\\U{scalar:08X}\"");
            let result = canonical_toml_key_path(&source, 16);
            if char::from_u32(scalar).is_none() {
                assert!(result.is_none());
                continue;
            }
            let canonical = result.expect("scalar fits");
            assert_eq!(
                canonical_toml_key_path(&canonical, 16).as_deref(),
                Some(canonical.as_ref())
            );
            if scalar <= 0xffff {
                let short = format!("\"\\u{scalar:04x}\"");
                assert_eq!(
                    canonical_toml_key_path(&short, 16).as_deref(),
                    Some(canonical.as_ref())
                );
            }
            if scalar <= 0xff {
                let byte = format!("\"\\x{scalar:02x}\"");
                assert_eq!(
                    canonical_toml_key_path(&byte, 16).as_deref(),
                    Some(canonical.as_ref())
                );
            }
        }
    }
}
