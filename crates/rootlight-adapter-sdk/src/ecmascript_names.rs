//! IdentifierName decoding shared by native and project ECMAScript lowering.

use std::borrow::Cow;

use unicode_general_category::{GeneralCategory, get_general_category};

/// Returns the exact code-point identity of a bounded ECMAScript identifier.
///
/// Unicode escapes are decoded without case folding or Unicode normalization.
/// Identifier properties follow Unicode 16.0, matching the pinned category data.
/// Keywords are allowed here: the native grammar owns binding-position rules.
/// Source spans must retain the authored spelling. Invalid characters, escapes,
/// excess source bytes or allocation failure return `None`, never a prefix.
#[must_use]
pub fn canonical_ecmascript_identifier(text: &str, maximum_bytes: usize) -> Option<Cow<'_, str>> {
    if text.is_empty() || text.len() > maximum_bytes {
        return None;
    }
    if !text.contains('\\') {
        let mut chars = text.chars();
        return (identifier_start(chars.next()?) && chars.all(identifier_continue))
            .then_some(Cow::Borrowed(text));
    }
    let mut decoded = String::new();
    decoded.try_reserve(text.len()).ok()?;
    let mut chars = text.chars();
    while let Some(mut ch) = chars.next() {
        if ch == '\\' {
            if chars.next()? != 'u' {
                return None;
            }
            let first = chars.next()?;
            let mut value = 0_u32;
            if first == '{' {
                let mut digits = false;
                loop {
                    let digit = chars.next()?;
                    if digit == '}' {
                        if !digits {
                            return None;
                        }
                        break;
                    }
                    value = value.checked_mul(16)?.checked_add(digit.to_digit(16)?)?;
                    digits = true;
                }
            } else {
                value = first.to_digit(16)?;
                for _ in 0..3 {
                    value = value
                        .checked_mul(16)?
                        .checked_add(chars.next()?.to_digit(16)?)?;
                }
            }
            // Identifier escapes denote code points, not UTF-16 string units;
            // adjacent surrogate escapes must not be combined into a character.
            ch = char::from_u32(value)?;
        }
        if !(if decoded.is_empty() {
            identifier_start(ch)
        } else {
            identifier_continue(ch)
        }) {
            return None;
        }
        decoded.push(ch);
    }
    Some(Cow::Owned(decoded))
}

fn identifier_start(ch: char) -> bool {
    if ch.is_ascii() {
        return ch.is_ascii_alphabetic() || matches!(ch, '$' | '_');
    }
    // Unicode 16 PropList Other_ID_Start supplements the letter categories.
    // U+2E2F is the sole letter-category member excluded by Pattern_Syntax.
    // https://www.unicode.org/Public/16.0.0/ucd/PropList.txt
    matches!(ch, '\u{1885}'..='\u{1886}' | '\u{2118}' | '\u{212e}' | '\u{309b}'..='\u{309c}')
        || (ch != '\u{2e2f}'
            && matches!(
                get_general_category(ch),
                GeneralCategory::UppercaseLetter
                    | GeneralCategory::LowercaseLetter
                    | GeneralCategory::TitlecaseLetter
                    | GeneralCategory::ModifierLetter
                    | GeneralCategory::OtherLetter
                    | GeneralCategory::LetterNumber
            ))
}

fn identifier_continue(ch: char) -> bool {
    if ch.is_ascii() {
        return ch.is_ascii_alphanumeric() || matches!(ch, '$' | '_');
    }
    identifier_start(ch)
        || matches!(ch, '\u{00b7}' | '\u{0387}' | '\u{1369}'..='\u{1371}' | '\u{19da}'
            | '\u{200c}'..='\u{200d}' | '\u{30fb}' | '\u{ff65}')
        || matches!(
            get_general_category(ch),
            GeneralCategory::NonspacingMark
                | GeneralCategory::SpacingMark
                | GeneralCategory::DecimalNumber
                | GeneralCategory::ConnectorPunctuation
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scalar_matches_unicode_16_identifier_properties() {
        // Fingerprint of ID_Start/ID_Continue from Unicode 16 DerivedCoreProperties,
        // with ECMAScript's dollar, underscore and joiner additions. The independent
        // table walk orders scalars numerically and hashes one two-bit role per scalar.
        // https://www.unicode.org/Public/16.0.0/ucd/DerivedCoreProperties.txt
        let mut fingerprint = 0xcbf2_9ce4_8422_2325_u64;
        let mut start_count = 0;
        let mut continue_count = 0;
        for ch in (0..=0x10_ffff).filter_map(char::from_u32) {
            let start = u8::from(identifier_start(ch));
            let continuation = u8::from(identifier_continue(ch));
            start_count += u32::from(start);
            continue_count += u32::from(continuation);
            fingerprint = (fingerprint ^ u64::from(start | (continuation << 1)))
                .wrapping_mul(0x0000_0100_0000_01b3);
        }
        assert_eq!((start_count, continue_count), (141_271, 144_542));
        assert_eq!(fingerprint, 0xf668_298e_f623_a476);
    }

    #[test]
    fn identifier_identity_decodes_only_unicode_escapes() {
        for (source, expected) in [
            (r"\u004cocal", "Local"),
            (r"L\u{0000006f}cal", "Local"),
            (r"\u{1d49c}", "𝒜"),
            (r"e\u0301", "e\u{301}"),
            (r"\u00e9", "é"),
            (r"a\u200c\u200d", "a\u{200c}\u{200d}"),
            (r"\u0024\u005f", "$_"),
        ] {
            assert_eq!(
                canonical_ecmascript_identifier(source, source.len()).as_deref(),
                Some(expected)
            );
            assert!(canonical_ecmascript_identifier(source, source.len() - 1).is_none());
        }
        for name in ["Local", "e\u{301}", "é", "𝒜", "default", "a\u{200c}"] {
            assert!(
                matches!(canonical_ecmascript_identifier(name, name.len()), Some(Cow::Borrowed(value)) if value == name)
            );
        }
        assert_ne!(
            canonical_ecmascript_identifier(r"e\u0301", 20),
            canonical_ecmascript_identifier(r"\u00e9", 20)
        );
    }

    #[test]
    fn malformed_identifier_never_returns_a_valid_prefix() {
        for name in [
            "",
            " a",
            "a ",
            "a.b",
            "1a",
            "a\0",
            "a\u{85}",
            "\u{301}a",
            "\u{2e2f}",
            "a\u{2e2f}",
            r"\x61",
            r"\u",
            r"\u{}",
            r"\u{61",
            r"\u006",
            r"\u006g",
            r"\u{110000}",
            r"\u{fffffffff}",
            r"\uD835\uDC9C",
            r"\ud800",
            r"a\udfff",
            r"\u0030a",
            r"a\u002e",
            r"a\u0020",
            r"a\u005c",
        ] {
            assert!(
                canonical_ecmascript_identifier(name, name.len()).is_none(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn unicode_identifier_exceptions_preserve_start_and_continue_roles() {
        assert_eq!(unicode_general_category::UNICODE_VERSION, (16, 0, 0));
        for ch in [
            '\u{1885}', '\u{1886}', '\u{2118}', '\u{212e}', '\u{309b}', '\u{309c}',
        ] {
            assert!(identifier_start(ch));
            assert!(identifier_continue(ch));
        }
        for ch in [
            '\u{b7}', '\u{387}', '\u{1369}', '\u{1371}', '\u{19da}', '\u{200c}', '\u{200d}',
            '\u{30fb}', '\u{ff65}', '\u{301}', '\u{203f}',
        ] {
            assert!(!identifier_start(ch));
            assert!(identifier_continue(ch));
        }
    }
}
