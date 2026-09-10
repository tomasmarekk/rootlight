//! Literal-only expressions used by Nix's parser-level attribute-name folding.
//! This recognizes string syntax, not variable values or general expressions;
//! callers retain the complete authored interpolation as source evidence.

use super::{AdapterError, Cow, append, decode_plain_name, skip_trivia};

// Nix's stripIndentation starts its minimum at this value, even for longer lines.
const INITIAL_INDENTATION: usize = 1_000_000;

pub(super) fn key<'a>(
    source: &'a str,
    maximum: usize,
    check: &mut impl FnMut() -> Result<(), AdapterError>,
) -> Result<Option<(Cow<'a, str>, &'a str)>, AdapterError> {
    check()?;
    let mut frames = Vec::new();
    let mut current = source;
    let (name, mut tail) = loop {
        check()?;
        let Some(mut rest) = current.strip_prefix("${").and_then(skip_trivia) else {
            return Ok(None);
        };
        let mut parentheses = 0usize;
        while let Some(inner) = rest.strip_prefix('(') {
            check()?;
            parentheses = parentheses
                .checked_add(1)
                .ok_or(crate::SinkError::AccountingOverflow)?;
            let Some(inner) = skip_trivia(inner) else {
                return Ok(None);
            };
            rest = inner;
        }
        if let Some(mut inner) = rest.strip_prefix("''") {
            if let Some(line) = inner.trim_start_matches(' ').strip_prefix('\n') {
                inner = line;
            }
            let trimmed = inner.trim_start_matches(' ');
            if trimmed.starts_with("${") {
                if inner.len() - trimmed.len() > INITIAL_INDENTATION {
                    return Ok(None);
                }
                // A sole ExprString inside an indented string is folded again.
                // Explicit frames avoid recursive calls on nested source input.
                frames
                    .try_reserve(1)
                    .map_err(|_| crate::SinkError::AllocationFailed)?;
                frames.push(parentheses);
                current = trimmed;
                continue;
            }
        }
        let Some((name, tail)) = literal(rest, maximum, check)? else {
            return Ok(None);
        };
        let Some(tail) = close_key(tail, parentheses, check)? else {
            return Ok(None);
        };
        break (name, tail);
    };
    while let Some(parentheses) = frames.pop() {
        check()?;
        let Some(rest) = tail.strip_prefix("''") else {
            return Ok(None);
        };
        let Some(rest) = close_key(rest, parentheses, check)? else {
            return Ok(None);
        };
        tail = rest;
    }
    Ok(Some((name, tail)))
}

fn close_key<'a>(
    tail: &'a str,
    parentheses: usize,
    check: &mut impl FnMut() -> Result<(), AdapterError>,
) -> Result<Option<&'a str>, AdapterError> {
    let Some(mut rest) = skip_trivia(tail) else {
        return Ok(None);
    };
    for _ in 0..parentheses {
        check()?;
        let Some(tail) = rest.strip_prefix(')').and_then(skip_trivia) else {
            return Ok(None);
        };
        rest = tail;
    }
    Ok(rest.strip_prefix('}'))
}

fn literal<'a>(
    source: &'a str,
    maximum: usize,
    check: &mut impl FnMut() -> Result<(), AdapterError>,
) -> Result<Option<(Cow<'a, str>, &'a str)>, AdapterError> {
    if let Some(inner) = source.strip_prefix('"') {
        let mut chars = inner.char_indices();
        while let Some((offset, character)) = chars.next() {
            check()?;
            if character == '\\' {
                chars.next();
            } else if character == '"' {
                let end = offset
                    .checked_add(2)
                    .ok_or(crate::SinkError::AccountingOverflow)?;
                let Some(written) = source.get(..end) else {
                    return Ok(None);
                };
                let Some(tail) = source.get(end..) else {
                    return Ok(None);
                };
                return Ok(decode_plain_name(written, maximum, check)?.map(|name| (name, tail)));
            }
        }
        return Ok(None);
    }
    if source.starts_with("''") {
        return indented(source, maximum, check);
    }
    let mut scheme = 0usize;
    for byte in source.bytes() {
        check()?;
        if !(byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')) {
            break;
        }
        scheme = scheme
            .checked_add(1)
            .ok_or(crate::SinkError::AccountingOverflow)?;
    }
    if !source
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        || source.as_bytes().get(scheme) != Some(&b':')
    {
        return Ok(None);
    }
    let start = scheme
        .checked_add(1)
        .ok_or(crate::SinkError::AccountingOverflow)?;
    let Some(body) = source.get(start..) else {
        return Ok(None);
    };
    let mut length = 0usize;
    for byte in body.bytes() {
        check()?;
        if !(byte.is_ascii_alphanumeric() || b"%/?:@&=+$,-_.!~*'".contains(&byte)) {
            break;
        }
        length = length
            .checked_add(1)
            .ok_or(crate::SinkError::AccountingOverflow)?;
    }
    if length == 0 {
        return Ok(None);
    }
    let end = start
        .checked_add(length)
        .ok_or(crate::SinkError::AccountingOverflow)?;
    Ok(source
        .get(..end)
        .filter(|name| name.len() <= maximum)
        .and_then(|name| source.get(end..).map(|tail| (Cow::Borrowed(name), tail))))
}

fn indented<'a>(
    source: &'a str,
    maximum: usize,
    check: &mut impl FnMut() -> Result<(), AdapterError>,
) -> Result<Option<(Cow<'a, str>, &'a str)>, AdapterError> {
    let Some(mut rest) = source.strip_prefix("''") else {
        return Ok(None);
    };
    if let Some(tail) = rest.trim_start_matches(' ').strip_prefix('\n') {
        rest = tail;
    }
    let mut pieces = Vec::<(&str, bool)>::new();
    let tail = loop {
        check()?;
        if rest.is_empty() || rest.starts_with("${") {
            return Ok(None);
        }
        let (piece, indentation, consumed) = if rest.starts_with("'''") {
            ("''", false, 3)
        } else if rest.starts_with("''$") {
            ("$", false, 3)
        } else if let Some(escaped) = rest.strip_prefix("''\\") {
            let Some(character) = escaped.chars().next() else {
                return Ok(None);
            };
            let length = character.len_utf8();
            let text = match character {
                'n' => "\n",
                'r' => "\r",
                't' => "\t",
                '\0' => return Ok(None),
                _ => match escaped.get(..length) {
                    Some(text) => text,
                    None => return Ok(None),
                },
            };
            (text, false, 3 + length)
        } else if let Some(tail) = rest.strip_prefix("''") {
            break tail;
        } else {
            // Match one IND_STR lexer token; escape tokens are not indentation.
            let mut length = 0usize;
            let mut chars = rest.char_indices().peekable();
            while let Some((offset, character)) = chars.next() {
                check()?;
                if character == '\0' {
                    return Ok(None);
                }
                if character == '$' || character == '\'' {
                    let next = chars.peek().map(|(_, c)| *c);
                    if character == '$'
                        && (next.is_none() || matches!(next, Some('{') | Some('\'')))
                        || character == '\''
                            && (next.is_none() || matches!(next, Some('\'') | Some('$')))
                    {
                        break;
                    }
                    if let Some((next_offset, next)) = chars.next() {
                        length = next_offset + next.len_utf8();
                        continue;
                    }
                }
                length = offset + character.len_utf8();
            }
            if length == 0 {
                let Some(character) = rest.chars().next() else {
                    return Ok(None);
                };
                let Some(text) = rest.get(..character.len_utf8()) else {
                    return Ok(None);
                };
                (text, false, character.len_utf8())
            } else {
                let Some(text) = rest.get(..length) else {
                    return Ok(None);
                };
                (text, true, length)
            }
        };
        if piece.contains('\0') {
            return Ok(None);
        }
        pieces
            .try_reserve(1)
            .map_err(|_| crate::SinkError::AllocationFailed)?;
        pieces.push((piece, indentation));
        let Some(tail) = rest.get(consumed..) else {
            return Ok(None);
        };
        rest = tail;
    };
    let mut minimum = INITIAL_INDENTATION;
    let mut at_start = true;
    let mut spaces = 0usize;
    for (piece, indentation) in &pieces {
        if !indentation {
            if at_start {
                minimum = minimum.min(spaces);
            }
            at_start = false;
            continue;
        }
        for character in piece.chars() {
            check()?;
            if at_start {
                if character == ' ' {
                    spaces += 1;
                } else if character == '\n' {
                    spaces = 0;
                } else {
                    minimum = minimum.min(spaces);
                    at_start = false;
                }
            } else if character == '\n' {
                at_start = true;
                spaces = 0;
            }
        }
    }
    at_start = true;
    spaces = 0;
    let mut value = None;
    for (index, (piece, _)) in pieces.iter().enumerate() {
        check()?;
        let mut output = String::new();
        for character in piece.chars() {
            check()?;
            if at_start && character == ' ' {
                let drop = spaces < minimum;
                spaces += 1;
                if drop {
                    continue;
                }
            } else if at_start && character != '\n' {
                at_start = false;
                spaces = 0;
            }
            if character == '\n' {
                at_start = true;
                spaces = 0;
            }
            let mut bytes = [0; 4];
            if append(&mut output, character.encode_utf8(&mut bytes), maximum).is_none() {
                return Ok(None);
            }
        }
        if index + 1 == pieces.len()
            && let Some(offset) = output.rfind('\n')
        {
            let start = offset + 1;
            if output
                .get(start..)
                .is_some_and(|line| line.bytes().all(|byte| byte == b' '))
            {
                output.truncate(start);
            }
        }
        if !output.is_empty() {
            // More than one retained token creates ExprConcatStrings, not ExprString.
            if value.is_some() {
                return Ok(None);
            }
            value = Some(output);
        }
    }
    Ok(Some((Cow::Owned(value.unwrap_or_default()), tail)))
}
