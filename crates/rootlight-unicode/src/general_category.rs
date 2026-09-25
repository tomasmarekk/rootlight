//! Unicode 16 general categories used by source identifiers and search terms.

use crate::general_category_data::{CATEGORY_WORDS, PAGE_IDS};

/// Unicode data version used for general-category classification.
pub const GENERAL_CATEGORY_UNICODE_VERSION: (u8, u8, u8) = (16, 0, 0);

/// General categories needed by Rootlight's source-language rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneralCategory {
    /// An uppercase letter.
    UppercaseLetter,
    /// A lowercase letter.
    LowercaseLetter,
    /// A titlecase letter.
    TitlecaseLetter,
    /// A modifier letter.
    ModifierLetter,
    /// Another letter.
    OtherLetter,
    /// A number with a letter-like form.
    LetterNumber,
    /// A nonspacing combining mark.
    NonspacingMark,
    /// A spacing combining mark.
    SpacingMark,
    /// A decimal digit.
    DecimalNumber,
    /// Connector punctuation.
    ConnectorPunctuation,
    /// A formatting character.
    Format,
    /// A currency symbol.
    CurrencySymbol,
    /// An enclosing combining mark.
    EnclosingMark,
    /// A category not distinguished by Rootlight's language rules.
    Other,
}

/// Classifies one scalar using the pinned Unicode 16 general-category data.
#[must_use]
pub fn get_general_category(character: char) -> GeneralCategory {
    let scalar = character as usize;
    let page = usize::from(PAGE_IDS[scalar >> 8]);
    let word = CATEGORY_WORDS[page * 16 + ((scalar >> 4) & 0xf)];
    let code = (word >> ((scalar & 0xf) * 4)) & 0xf;
    match code {
        1 => GeneralCategory::UppercaseLetter,
        2 => GeneralCategory::LowercaseLetter,
        3 => GeneralCategory::TitlecaseLetter,
        4 => GeneralCategory::ModifierLetter,
        5 => GeneralCategory::OtherLetter,
        6 => GeneralCategory::LetterNumber,
        7 => GeneralCategory::NonspacingMark,
        8 => GeneralCategory::SpacingMark,
        9 => GeneralCategory::DecimalNumber,
        10 => GeneralCategory::ConnectorPunctuation,
        11 => GeneralCategory::Format,
        12 => GeneralCategory::CurrencySymbol,
        13 => GeneralCategory::EnclosingMark,
        _ => GeneralCategory::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_pages_are_bounded_and_cover_scalar_boundaries() {
        assert_eq!(GENERAL_CATEGORY_UNICODE_VERSION, (16, 0, 0));
        assert_eq!(PAGE_IDS.len(), 4352);
        assert_eq!(CATEGORY_WORDS.len(), 136 * 16);
        for page in PAGE_IDS {
            assert!(usize::from(*page) * 16 < CATEGORY_WORDS.len());
        }
        for word in CATEGORY_WORDS {
            for nibble in 0..16 {
                assert!((word >> (nibble * 4)) & 0xf <= 13);
            }
        }
        assert_eq!(get_general_category('\0'), GeneralCategory::Other);
        assert_eq!(get_general_category('A'), GeneralCategory::UppercaseLetter);
        assert_eq!(get_general_category('a'), GeneralCategory::LowercaseLetter);
        assert_eq!(
            get_general_category('\u{0301}'),
            GeneralCategory::NonspacingMark
        );
        assert_eq!(
            get_general_category('\u{20dd}'),
            GeneralCategory::EnclosingMark
        );
        assert_eq!(
            get_general_category('\u{2160}'),
            GeneralCategory::LetterNumber
        );
        assert_eq!(get_general_category('\u{10ffff}'), GeneralCategory::Other);
    }

    #[test]
    fn every_scalar_matches_the_pinned_unicode_16_fingerprint() {
        // Measured only after exhaustive equality with the previous pinned
        // Unicode 16 classifier; the external library is not a build input.
        let mut fingerprint = 0xcbf29ce484222325_u64;
        for scalar in 0..=0x10ffff {
            let Some(character) = char::from_u32(scalar) else {
                continue;
            };
            let code = match get_general_category(character) {
                GeneralCategory::Other => 0,
                GeneralCategory::UppercaseLetter => 1,
                GeneralCategory::LowercaseLetter => 2,
                GeneralCategory::TitlecaseLetter => 3,
                GeneralCategory::ModifierLetter => 4,
                GeneralCategory::OtherLetter => 5,
                GeneralCategory::LetterNumber => 6,
                GeneralCategory::NonspacingMark => 7,
                GeneralCategory::SpacingMark => 8,
                GeneralCategory::DecimalNumber => 9,
                GeneralCategory::ConnectorPunctuation => 10,
                GeneralCategory::Format => 11,
                GeneralCategory::CurrencySymbol => 12,
                GeneralCategory::EnclosingMark => 13,
            };
            fingerprint = (fingerprint ^ code).wrapping_mul(0x100000001b3);
        }
        assert_eq!(fingerprint, 0x77210e22aea8920a);
    }
}
