//! Recognize written Modelica headers without treating the shared `.mo` suffix as proof.
//! This bounded lexical certificate selects a language only; the native parser
//! remains responsible for syntax, declarations and analysis gaps.

use crate::MAX_CLASSIFICATION_BYTES;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Word(&'a str),
    QuotedIdentifier(&'a str),
    Symbol(u8),
    String,
}

/// Returns a written header certificate from a bounded text prefix.
///
/// The caller validates the whole source encoding. A truncated body preserves
/// a complete `within` declaration and class header; cut declaration tokens
/// never supply this signal. Complete files retain the stricter lexical check.
pub(super) fn has_content_hint(content: &[u8], prefix_truncated: bool) -> bool {
    let sample = &content[..content.len().min(MAX_CLASSIFICATION_BYTES)];
    let prefix_truncated = prefix_truncated || sample.len() < content.len();
    let text = match std::str::from_utf8(sample) {
        Ok(text) => text,
        Err(error) if prefix_truncated && error.error_len().is_none() => {
            let Ok(text) = std::str::from_utf8(&sample[..error.valid_up_to()]) else {
                return false;
            };
            text
        }
        Err(_) => return false,
    };
    let Some(tokens) = tokens(
        text.strip_prefix('\u{feff}').unwrap_or(text),
        prefix_truncated,
    ) else {
        return false;
    };
    let mut cursor = 0;
    let within = tokens.first() == Some(&Token::Word("within"));
    if within {
        cursor += 1;
        if tokens.get(cursor) != Some(&Token::Symbol(b';')) {
            if !identifier(tokens.get(cursor).copied()) {
                return false;
            }
            cursor += 1;
            while tokens.get(cursor) == Some(&Token::Symbol(b'.')) {
                cursor += 1;
                if !identifier(tokens.get(cursor).copied()) {
                    return false;
                }
                cursor += 1;
            }
        }
        if tokens.get(cursor) != Some(&Token::Symbol(b';')) {
            return false;
        }
        cursor += 1;
    }
    while matches!(
        tokens.get(cursor),
        Some(Token::Word(
            "encapsulated" | "partial" | "final" | "redeclare" | "replaceable" | "inner" | "outer"
        ))
    ) {
        cursor += 1;
    }
    let class_keyword = match tokens.get(cursor) {
        Some(Token::Word("pure" | "impure")) => {
            cursor += 1;
            tokens.get(cursor) == Some(&Token::Word("function"))
        }
        Some(Token::Word("expandable")) => {
            cursor += 1;
            tokens.get(cursor) == Some(&Token::Word("connector"))
        }
        Some(Token::Word("operator")) => {
            if matches!(
                tokens.get(cursor + 1),
                Some(Token::Word("record" | "function"))
            ) {
                cursor += 1;
            }
            true
        }
        Some(Token::Word(
            "class" | "model" | "block" | "record" | "connector" | "type" | "package" | "function",
        )) => true,
        _ => false,
    };
    if !class_keyword || !identifier(tokens.get(cursor + 1).copied()) {
        return false;
    }
    if prefix_truncated {
        return within;
    }
    let name = tokens[cursor + 1];
    // `within` is a distinct source-language declaration. Without it, require
    // the written class terminator so a generic class header cannot admit `.mo`.
    within
        || tokens[cursor + 2..]
            .windows(3)
            .any(|window| window == [Token::Word("end"), name, Token::Symbol(b';')])
}

fn identifier(token: Option<Token<'_>>) -> bool {
    match token {
        Some(Token::QuotedIdentifier(_)) => true,
        Some(Token::Word(word)) => !matches!(
            word,
            "algorithm"
                | "and"
                | "annotation"
                | "block"
                | "break"
                | "class"
                | "connect"
                | "connector"
                | "constant"
                | "constrainedby"
                | "der"
                | "discrete"
                | "each"
                | "else"
                | "elseif"
                | "elsewhen"
                | "encapsulated"
                | "end"
                | "enumeration"
                | "equation"
                | "expandable"
                | "extends"
                | "external"
                | "false"
                | "final"
                | "flow"
                | "for"
                | "function"
                | "if"
                | "impure"
                | "import"
                | "in"
                | "initial"
                | "inner"
                | "input"
                | "loop"
                | "model"
                | "not"
                | "operator"
                | "or"
                | "outer"
                | "output"
                | "package"
                | "parameter"
                | "partial"
                | "protected"
                | "public"
                | "pure"
                | "record"
                | "redeclare"
                | "replaceable"
                | "return"
                | "stream"
                | "then"
                | "true"
                | "type"
                | "when"
                | "while"
                | "within"
        ),
        _ => false,
    }
}

fn tokens(mut text: &str, prefix_truncated: bool) -> Option<Vec<Token<'_>>> {
    let mut output = Vec::new();
    while !text.is_empty() {
        let first = text.chars().next()?;
        if first.is_ascii_whitespace() {
            text = &text[first.len_utf8()..];
        } else if let Some(comment) = text.strip_prefix("//") {
            text = comment
                .find(['\r', '\n'])
                .map_or("", |end| &comment[end + 1..]);
        } else if let Some(comment) = text.strip_prefix("/*") {
            let Some(end) = comment.find("*/") else {
                return prefix_truncated.then_some(output);
            };
            text = &comment[end + 2..];
        } else if first == '"' || first == '\'' {
            let Some(end) = quoted_end(text, first) else {
                return prefix_truncated.then_some(output);
            };
            output.push(if first == '\'' {
                if end == 2 {
                    return None;
                }
                Token::QuotedIdentifier(&text[..end])
            } else {
                Token::String
            });
            text = &text[end..];
        } else if first.is_ascii_alphabetic() || first == '_' {
            let end = text
                .find(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .unwrap_or(text.len());
            if prefix_truncated && end == text.len() {
                break;
            }
            output.push(Token::Word(&text[..end]));
            text = &text[end..];
        } else {
            output.push(Token::Symbol(if first.is_ascii() {
                u8::try_from(u32::from(first)).ok()?
            } else {
                0
            }));
            text = &text[first.len_utf8()..];
        }
    }
    Some(output)
}

fn quoted_end(text: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (offset, character) in text.char_indices().skip(1) {
        if quote == '\'' && matches!(character, '\n' | '\r') {
            return None;
        }
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            return Some(offset + character.len_utf8());
        }
    }
    None
}
