//! Allocation-free Unicode 16 identifier properties for native source captures.
//! This module classifies scalars only; each language owns punctuation, complete
//! name rules, reserved words, capture context and semantic interpretation.
//!
//! Range data comes from the retained `unicode-ident-1.0.14` Unicode 16 tables.
//! Its unchanged Unicode notice is in `data/XID-16.0.0-LICENSE.txt`.

#[path = "xid16_ranges.rs"]
mod data;

/// Unicode data version used for XID properties.
pub const XID_UNICODE_VERSION: (u8, u8, u8) = (16, 0, 0);

/// Reports whether a scalar has the Unicode 16 `XID_Start` property.
#[must_use]
pub fn is_xid_start(character: char) -> bool {
    contains(data::XID_START, character)
}

/// Reports whether a scalar has the Unicode 16 `XID_Continue` property.
#[must_use]
pub fn is_xid_continue(character: char) -> bool {
    contains(data::XID_CONTINUE, character)
}

fn contains(ranges: &[(u32, u32)], character: char) -> bool {
    let scalar = u32::from(character);
    let position = ranges.partition_point(|&(start, _)| start <= scalar);
    position
        .checked_sub(1)
        .and_then(|previous| ranges.get(previous))
        .is_some_and(|&(_, end)| scalar <= end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_range_data_is_ordered_bounded_and_distinguishes_start() {
        assert_eq!(XID_UNICODE_VERSION, (16, 0, 0));
        for ranges in [data::XID_START, data::XID_CONTINUE] {
            for &(start, end) in ranges {
                assert!(start <= end && end <= 0x10ffff);
                assert!(end < 0xd800 || start > 0xdfff);
            }
            assert!(ranges.windows(2).all(|pair| pair[0].1 < pair[1].0));
        }
        assert!(is_xid_start('Δ'));
        assert!(!is_xid_start('_'));
        assert!(is_xid_continue('_'));
        assert!(!is_xid_start('\u{301}'));
        assert!(is_xid_continue('\u{301}'));
        for character in ['\0', ' ', '.', '🛰', '\u{10ffff}'] {
            assert!(!is_xid_start(character));
            assert!(!is_xid_continue(character));
        }
    }
}
