//! Bounded canonical JSON key literals without object-to-map conversion.
//! Quoting preserves empty and control-bearing keys in nonempty IR names while
//! retaining unpaired UTF-16 units that cannot be represented by Rust strings.

use std::{borrow::Cow, str::Chars};

pub(crate) fn canonical_json_key(text: &str, maximum_bytes: usize) -> Option<Cow<'_, str>> {
    if text.len() > maximum_bytes {
        return None;
    }
    let body = text.strip_prefix('"')?.strip_suffix('"')?;
    let mut units = Units {
        characters: body.chars(),
        pending: None,
    }
    .peekable();
    let mut output = String::new();
    append(&mut output, "\"", maximum_bytes)?;
    while let Some(unit) = units.next() {
        let unit = unit.ok()?;
        if (0xd800..=0xdbff).contains(&unit)
            && let Some(Ok(low)) = units.peek().copied()
            && (0xdc00..=0xdfff).contains(&low)
        {
            units.next();
            let scalar = 0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
            append_character(&mut output, char::from_u32(scalar)?, maximum_bytes)?;
        } else if (0xd800..=0xdfff).contains(&unit) {
            append_unit_escape(&mut output, unit, maximum_bytes)?;
        } else {
            append_character(&mut output, char::from_u32(u32::from(unit))?, maximum_bytes)?;
        }
    }
    append(&mut output, "\"", maximum_bytes)?;
    if output == text {
        Some(Cow::Borrowed(text))
    } else {
        Some(Cow::Owned(output))
    }
}

struct Units<'a> {
    characters: Chars<'a>,
    pending: Option<u16>,
}

pub(crate) fn display_json_key(canonical: &str) -> Option<Cow<'_, str>> {
    let body = canonical.strip_prefix('"')?.strip_suffix('"')?;
    if body.is_empty() || body.trim() != body || body.chars().any(char::is_control) {
        return None;
    }
    if !body.contains('\\') {
        return (!body.contains('"')).then_some(Cow::Borrowed(body));
    }
    // Canonical keys have already decoded scalar escapes. The remaining
    // Unicode escapes represent controls or unpaired UTF-16 units; keep them
    // quoted so display text cannot misrepresent their identity.
    let mut decoded = String::new();
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        let character = if character == '\\' {
            match characters.next()? {
                '"' => '"',
                '\\' => '\\',
                _ => return None,
            }
        } else {
            if character == '"' {
                return None;
            }
            character
        };
        decoded.try_reserve(character.len_utf8()).ok()?;
        decoded.push(character);
    }
    Some(Cow::Owned(decoded))
}

impl Iterator for Units<'_> {
    type Item = Result<u16, ()>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(unit) = self.pending.take() {
            return Some(Ok(unit));
        }
        let character = self.characters.next()?;
        if character == '\\' {
            return Some(self.escape());
        }
        if character == '"' || u32::from(character) < 0x20 {
            return Some(Err(()));
        }
        let mut buffer = [0; 2];
        let units = character.encode_utf16(&mut buffer);
        self.pending = units.get(1).copied();
        Some(Ok(*units.first()?))
    }
}

impl Units<'_> {
    fn escape(&mut self) -> Result<u16, ()> {
        Ok(match self.characters.next().ok_or(())? {
            '"' => u16::from(b'"'),
            '\\' => u16::from(b'\\'),
            '/' => u16::from(b'/'),
            'b' => 8,
            'f' => 12,
            'n' => 10,
            'r' => 13,
            't' => 9,
            'u' => {
                let mut unit = 0u16;
                for _ in 0..4 {
                    let digit = self
                        .characters
                        .next()
                        .and_then(|value| value.to_digit(16))
                        .ok_or(())?;
                    unit = unit
                        .checked_mul(16)
                        .and_then(|value| value.checked_add(u16::try_from(digit).ok()?))
                        .ok_or(())?;
                }
                unit
            }
            _ => return Err(()),
        })
    }
}

/// Appends canonical data-key text without exceeding the caller's byte budget.
pub(crate) fn append(output: &mut String, text: &str, maximum_bytes: usize) -> Option<()> {
    if output.len().checked_add(text.len())? > maximum_bytes {
        return None;
    }
    output.try_reserve(text.len()).ok()?;
    output.push_str(text);
    Some(())
}

/// Uses one injective quoted scalar spelling for JSON, TOML and YAML data keys.
pub(crate) fn append_character(
    output: &mut String,
    character: char,
    maximum_bytes: usize,
) -> Option<()> {
    match character {
        '"' => append(output, "\\\"", maximum_bytes),
        '\\' => append(output, "\\\\", maximum_bytes),
        character if character.is_control() => {
            // Escaping all controls keeps names safe without conflating a
            // control with a literal backslash-u sequence in another key.
            let mut buffer = [0; 2];
            for unit in character.encode_utf16(&mut buffer) {
                append_unit_escape(output, *unit, maximum_bytes)?;
            }
            Some(())
        }
        character => append(output, character.encode_utf8(&mut [0; 4]), maximum_bytes),
    }
}

fn append_unit_escape(output: &mut String, unit: u16, maximum_bytes: usize) -> Option<()> {
    append(output, "\\u", maximum_bytes)?;
    for shift in [12, 8, 4, 0] {
        let digit = char::from_digit(u32::from((unit >> shift) & 0xf), 16)?;
        append(output, digit.encode_utf8(&mut [0; 4]), maximum_bytes)?;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_keys_share_canonical_identity_without_losing_units() {
        for (source, expected) in [
            (r#""a""#, r#""a""#),
            (r#""\u0061""#, r#""a""#),
            (r#""\/""#, r#""/""#),
            (r#""\n""#, r#""\u000a""#),
            (r#""\uD83C\uDF0D""#, r#""🌍""#),
            (r#""\uD800""#, r#""\ud800""#),
            (r#""\uDC00""#, r#""\udc00""#),
            (r#""\ud800\u0061""#, r#""\ud800a""#),
            (r#""\ud800\ud800""#, r#""\ud800\ud800""#),
            (r#""""#, r#""""#),
            (r#"" a/b~c ""#, r#"" a/b~c ""#),
            (r#""\u007f""#, r#""\u007f""#),
        ] {
            let result = canonical_json_key(source, 128).expect("key is representable");
            assert_eq!(result, expected);
            assert_eq!(canonical_json_key(&result, 128).as_deref(), Some(expected));
        }
    }

    #[test]
    fn distinct_keys_do_not_collapse_or_normalize_unicode() {
        for (left, right) in [
            (r#""\u0000""#, r#""\\u0000""#),
            (r#""""#, r#"" ""#),
            (r#""\ud800""#, r#""�""#),
            (r#""\ud800""#, r#""\\ud800""#),
            (r#""é""#, r#""e\u0301""#),
            (r#""a/b""#, r#""a~1b""#),
        ] {
            assert_ne!(
                canonical_json_key(left, 128),
                canonical_json_key(right, 128)
            );
        }
    }

    #[test]
    fn malformed_captures_and_both_byte_limits_are_rejected() {
        for source in [
            "",
            "a",
            "\"",
            "\"a",
            "a\"",
            "\"a\"b\"",
            "\"\\u\"",
            "\"\\u123z\"",
            "\"\\x20\"",
            "\"\\\"",
            "\"\n\"",
            "\"\0\"",
        ] {
            assert!(canonical_json_key(source, 128).is_none(), "{source:?}");
        }
        assert_eq!(canonical_json_key(r#""a""#, 3).as_deref(), Some(r#""a""#));
        assert!(canonical_json_key(r#""a""#, 2).is_none());
        assert!(canonical_json_key(r#""\u0061""#, 7).is_none());
        assert!(canonical_json_key("\"\u{85}\"", 7).is_none());
        assert_eq!(
            canonical_json_key("\"\u{85}\"", 8).as_deref(),
            Some(r#""\u0085""#)
        );
    }

    #[test]
    fn every_utf16_unit_has_idempotent_lossless_canonical_spelling() {
        for unit in 0u16..=u16::MAX {
            let source = format!("\"\\u{unit:04X}\"");
            let canonical = canonical_json_key(&source, 8).expect("one unit fits");
            assert_eq!(
                canonical_json_key(&canonical, 8).as_deref(),
                Some(canonical.as_ref())
            );
            let body = canonical
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .expect("quoted key");
            let decoded = Units {
                characters: body.chars(),
                pending: None,
            }
            .collect::<Result<Vec<_>, _>>()
            .expect("canonical units");
            assert_eq!(decoded, [unit], "{source}");
        }
    }
}
