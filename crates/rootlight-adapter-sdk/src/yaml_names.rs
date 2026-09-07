//! Bounded YAML 1.2 Core flow-scalar identities for structural key captures.
//! Type prefixes prevent quoted strings from merging with implicit non-string
//! keys; document-dependent tags, aliases and block styles are not guessed here.

use std::{iter::Peekable, str::Chars};

use crate::json_names::{append, append_character};

mod block;
mod context;
mod numbers;

pub use block::YamlBlockScalar;
pub use context::{YamlDocumentContext, YamlScalarIdentity};

pub(crate) fn anchor_name(text: &str, maximum: usize) -> Option<&str> {
    (!text.is_empty()
        && text.len() <= maximum
        && text.chars().all(|character| {
            printable(character)
                && !white(character)
                && !matches!(character, '\u{feff}' | '[' | ']' | '{' | '}' | ',')
        }))
    .then_some(text)
}

pub(crate) fn canonical_flow_key(text: &str, maximum: usize) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let (value, plain) = decode_flow(text, maximum)?;
    canonical_value(&value, plain, maximum)
}

fn decode_flow(text: &str, maximum: usize) -> Option<(String, bool)> {
    if text.len() > maximum {
        return None;
    }
    if text.is_empty() {
        return Some((String::new(), true));
    }
    let (body, quote) = match text.chars().next()? {
        quote @ ('\'' | '"') => (text.strip_prefix(quote)?.strip_suffix(quote)?, Some(quote)),
        _ => {
            validate_plain(text)?;
            (text, None)
        }
    };
    Some((decode(body, quote, maximum)?, quote.is_none()))
}

fn canonical_value(value: &str, plain: bool, maximum: usize) -> Option<String> {
    if plain {
        if let Some(value) = match value {
            "" | "~" | "null" | "Null" | "NULL" => Some("null:null"),
            "true" | "True" | "TRUE" => Some("bool:true"),
            "false" | "False" | "FALSE" => Some("bool:false"),
            _ => None,
        } {
            let mut result = String::new();
            append(&mut result, value, maximum)?;
            return Some(result);
        }
        // Classification is separate from normalization failure: a numeric
        // value exceeding the output budget must never become a string key.
        if let Some(number) = numbers::classify(value) {
            return number.canonical(maximum);
        }
    }
    let mut result = String::new();
    append(&mut result, "str:", maximum)?;
    append_quoted(&mut result, value, maximum)?;
    Some(result)
}

fn append_quoted(result: &mut String, value: &str, maximum: usize) -> Option<()> {
    append(result, "\"", maximum)?;
    for character in value.chars() {
        append_character(result, character, maximum)?;
    }
    append(result, "\"", maximum)
}

fn printable(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\r' | '\u{20}'..='\u{7e}' | '\u{85}' |
        '\u{a0}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}

fn white(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r')
}

fn validate_plain(text: &str) -> Option<()> {
    if text.starts_with(white) || text.ends_with(white) {
        return None;
    }
    let mut characters = text.chars().peekable();
    let first = characters.next()?;
    if matches!(
        first,
        ',' | '['
            | ']'
            | '{'
            | '}'
            | '#'
            | '&'
            | '*'
            | '!'
            | '|'
            | '>'
            | '\''
            | '"'
            | '%'
            | '@'
            | '`'
    ) || (matches!(first, '-' | '?' | ':') && characters.peek().is_none_or(|c| white(*c)))
    {
        return None;
    }
    let mut previous = None;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if !printable(character)
            || (character == '#' && previous.is_none_or(white))
            || (character == ':' && characters.peek().is_none_or(|c| white(*c)))
        {
            return None;
        }
        previous = Some(character);
    }
    // Flow-out scalars may contain collection punctuation. Whether that spelling
    // is legal in this key's context is the native grammar's responsibility.
    Some(())
}

fn decode(body: &str, quote: Option<char>, maximum: usize) -> Option<String> {
    let mut characters = body.chars().peekable();
    let mut result = String::new();
    let mut pending = String::new();
    while let Some(character) = characters.next() {
        if !printable(character) {
            return None;
        }
        if matches!(character, ' ' | '\t') {
            append(&mut pending, character.encode_utf8(&mut [0; 4]), maximum)?;
            continue;
        }
        if matches!(character, '\r' | '\n') {
            pending.clear();
            fold(&mut characters, character, false, &mut result, maximum)?;
            continue;
        }
        append(&mut result, &pending, maximum)?;
        pending.clear();
        let character = match (quote, character) {
            (Some('\''), '\'') => {
                if characters.next()? != '\'' {
                    return None;
                }
                '\''
            }
            (Some('"'), '"') => return None,
            (Some('"'), '\\') => {
                let marker = characters.next()?;
                if matches!(marker, '\r' | '\n') {
                    fold(&mut characters, marker, true, &mut result, maximum)?;
                    continue;
                }
                escape(&mut characters, marker)?
            }
            _ => character,
        };
        append(&mut result, character.encode_utf8(&mut [0; 4]), maximum)?;
    }
    append(&mut result, &pending, maximum)?;
    Some(result)
}

fn fold(
    characters: &mut Peekable<Chars<'_>>,
    first: char,
    escaped: bool,
    output: &mut String,
    maximum: usize,
) -> Option<()> {
    if first == '\r' {
        characters.next_if_eq(&'\n');
    }
    let mut empty_lines = 0usize;
    loop {
        while characters.next_if(|c| matches!(c, ' ' | '\t')).is_some() {}
        match characters.peek().copied() {
            Some('\r' | '\n') => {
                let line_break = characters.next()?;
                if line_break == '\r' {
                    characters.next_if_eq(&'\n');
                }
                empty_lines = empty_lines.checked_add(1)?;
            }
            _ => break,
        }
    }
    if empty_lines == 0 && !escaped {
        append(output, " ", maximum)?;
    }
    for _ in 0..empty_lines {
        append(output, "\n", maximum)?;
    }
    Some(())
}

fn escape(characters: &mut Peekable<Chars<'_>>, marker: char) -> Option<char> {
    match marker {
        '0' => Some('\0'),
        'a' => Some('\u{7}'),
        'b' => Some('\u{8}'),
        't' | '\t' => Some('\t'),
        'n' => Some('\n'),
        'v' => Some('\u{b}'),
        'f' => Some('\u{c}'),
        'r' => Some('\r'),
        'e' => Some('\u{1b}'),
        ' ' => Some(' '),
        '"' => Some('"'),
        '/' => Some('/'),
        '\\' => Some('\\'),
        'N' => Some('\u{85}'),
        '_' => Some('\u{a0}'),
        'L' => Some('\u{2028}'),
        'P' => Some('\u{2029}'),
        'x' | 'u' | 'U' => {
            let count = match marker {
                'x' => 2,
                'u' => 4,
                _ => 8,
            };
            let mut scalar = 0u32;
            for _ in 0..count {
                scalar = scalar
                    .checked_mul(16)?
                    .checked_add(characters.next()?.to_digit(16)?)?;
            }
            // YAML escapes encode Unicode scalars, not UTF-16 surrogate pairs.
            char::from_u32(scalar)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
