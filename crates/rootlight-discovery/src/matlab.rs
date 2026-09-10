//! Bounded MATLAB routing hints, with script-only evidence for the shared .m suffix.
//! Comments and literals are lexical boundaries, never keyword evidence. This is
//! language selection, not validation of the program or its runtime semantics.

use super::objective_c;

#[derive(Default)]
pub(super) struct Hints {
    pub(super) declaration: bool,
    pub(super) script: bool,
}

pub(super) fn hints(mut source: &[u8]) -> Hints {
    source = source.strip_prefix(b"\xef\xbb\xbf").unwrap_or(source);
    let mut result = Hints::default();
    let mut statement_start = true;
    let mut expression_end = false;
    let mut code_seen = false;
    let mut terminated = true;
    let mut assignment = false;
    while let Some((&first, rest)) = source.split_first() {
        if first.is_ascii_whitespace() {
            statement_start |= matches!(first, b'\r' | b'\n');
            expression_end = false;
            source = rest;
            continue;
        }
        if let Some(comment) = source.strip_prefix(b"/*") {
            let Some(end) = comment.windows(2).position(|pair| pair == b"*/") else {
                break;
            };
            source = comment.get(end + 2..).unwrap_or_default();
            continue;
        }
        if let Some(comment) = source.strip_prefix(b"//") {
            source = objective_c::after_line_comment(comment);
            statement_start = true;
            expression_end = false;
            continue;
        }
        if source.starts_with(b"...") {
            source = after_line(source);
            expression_end = false;
            continue;
        }
        if first == b'%' {
            result.script |= statement_start && (!code_seen || terminated);
            source = if statement_start && line(source).trim_ascii() == b"%{" {
                after_block_comment(after_line(source))
            } else {
                after_line(source)
            };
            statement_start = true;
            expression_end = false;
            continue;
        }
        if first == b'#'
            || [b"@interface".as_slice(), b"@implementation".as_slice()]
                .iter()
                .any(|keyword| {
                    source.strip_prefix(*keyword).is_some_and(|tail| {
                        tail.first().is_some_and(u8::is_ascii_whitespace)
                            || tail.starts_with(b"/*")
                            || tail.starts_with(b"//")
                    })
                })
            || [
                b"R\"".as_slice(),
                b"u8R\"".as_slice(),
                b"uR\"".as_slice(),
                b"UR\"".as_slice(),
                b"LR\"".as_slice(),
            ]
            .iter()
            .any(|prefix| source.starts_with(prefix))
        {
            return Hints::default();
        }
        code_seen = true;
        if first == b'[' && assignment {
            result.script |= numeric_matrix(rest);
        }
        assignment = first == b'=';
        terminated = first == b';';
        if matches!(first, b'\'' | b'"') && !(first == b'\'' && expression_end) {
            let matlab = after_quoted(rest, first);
            let c = objective_c::after_quoted(rest, first);
            // A C escape and a MATLAB literal backslash can disagree about the
            // closing quote. Do not mine either interpretation's literal body.
            source = if matlab.len() < c.len() { matlab } else { c };
            statement_start = false;
            expression_end = true;
            continue;
        }
        if first.is_ascii_digit() {
            let tail = objective_c::after_number(rest);
            let number = source.get(..source.len() - tail.len()).unwrap_or_default();
            result.script |= number.ends_with(b".")
                && tail
                    .first()
                    .is_some_and(|byte| matches!(byte, b'*' | b'/' | b'\\' | b'^' | b'\''));
            source = tail;
            statement_start = false;
            expression_end = true;
            continue;
        }
        if identifier_byte(first) {
            let length = source
                .iter()
                .take_while(|byte| identifier_byte(**byte))
                .count();
            let word = source.get(..length).unwrap_or_default();
            let tail = source.get(length..).unwrap_or_default();
            if statement_start {
                let separated = tail.first().is_some_and(u8::is_ascii_whitespace);
                let next = after_header_continuations(tail).first().copied();
                result.declaration |= separated
                    && match word {
                        b"function" => {
                            next.is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'[')
                        }
                        b"classdef" => {
                            next.is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'(')
                        }
                        _ => false,
                    };
                result.script |=
                    matches!(word, b"for" | b"parfor") && separated && assignment_follows(tail);
            }
            source = tail;
            statement_start = false;
            expression_end = true;
            continue;
        }
        if first == b'.'
            && rest
                .first()
                .is_some_and(|byte| matches!(byte, b'*' | b'/' | b'\\' | b'^' | b'\''))
        {
            result.script = true;
        }
        statement_start = matches!(first, b';' | b',');
        expression_end = matches!(first, b')' | b']' | b'}' | b'\'');
        source = rest;
    }
    result
}

fn after_header_continuations(mut source: &[u8]) -> &[u8] {
    loop {
        source = source.trim_ascii_start();
        if !source.starts_with(b"...") {
            return source;
        }
        source = after_line(source);
    }
}

fn numeric_matrix(mut source: &[u8]) -> bool {
    let mut value_seen = false;
    loop {
        source = source.trim_ascii_start();
        if value_seen && source.starts_with(b"]") {
            return true;
        }
        if value_seen
            && source
                .first()
                .is_some_and(|byte| matches!(byte, b',' | b';' | b':'))
        {
            source = source.get(1..).unwrap_or_default().trim_ascii_start();
        }
        if source
            .first()
            .is_some_and(|byte| matches!(byte, b'+' | b'-'))
        {
            source = source.get(1..).unwrap_or_default();
        }
        let length = source
            .iter()
            .take_while(|byte| byte.is_ascii_digit() || **byte == b'.')
            .count();
        if length == 0
            || !source
                .get(..length)
                .unwrap_or_default()
                .iter()
                .any(u8::is_ascii_digit)
        {
            return false;
        }
        source = source.get(length..).unwrap_or_default();
        value_seen = true;
    }
}

fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || !byte.is_ascii()
}

fn assignment_follows(source: &[u8]) -> bool {
    let source = source.trim_ascii_start();
    if !source.first().is_some_and(u8::is_ascii_alphabetic) {
        return false;
    }
    let length = source
        .iter()
        .take_while(|byte| identifier_byte(**byte))
        .count();
    source
        .get(length..)
        .unwrap_or_default()
        .trim_ascii_start()
        .strip_prefix(b"=")
        .is_some_and(|tail| !tail.starts_with(b"="))
}

fn line(source: &[u8]) -> &[u8] {
    source
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default()
}

fn after_line(source: &[u8]) -> &[u8] {
    source
        .iter()
        .position(|byte| *byte == b'\n')
        .and_then(|end| source.get(end + 1..))
        .unwrap_or_default()
}

fn after_block_comment(mut source: &[u8]) -> &[u8] {
    let mut depth = 1usize;
    while !source.is_empty() {
        match line(source).trim_ascii() {
            b"%{" => {
                let Some(next) = depth.checked_add(1) else {
                    return &[];
                };
                depth = next;
            }
            b"%}" => {
                depth -= 1;
                if depth == 0 {
                    return after_line(source);
                }
            }
            _ => {}
        }
        source = after_line(source);
    }
    &[]
}

fn after_quoted(mut source: &[u8], delimiter: u8) -> &[u8] {
    while let Some((&first, rest)) = source.split_first() {
        if first == delimiter {
            if let Some(rest) = rest.strip_prefix(&[delimiter]) {
                source = rest;
                continue;
            }
            return rest;
        }
        source = rest;
    }
    &[]
}
