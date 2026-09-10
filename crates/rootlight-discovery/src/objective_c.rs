//! Bounded Objective-C content hints outside C-family comments and literals.
//! These hints select source routing; they do not validate or preprocess code.

pub(super) fn has_content_hint(mut source: &[u8]) -> bool {
    while let Some((&first, rest)) = source.split_first() {
        if let Some(comment) = source.strip_prefix(b"/*") {
            let Some(end) = comment.windows(2).position(|pair| pair == b"*/") else {
                return false;
            };
            source = comment.get(end + 2..).unwrap_or_default();
        } else if let Some(comment) = source.strip_prefix(b"//") {
            source = after_line_comment(comment);
        } else if let Some(raw) = [
            b"R\"".as_slice(),
            b"u8R\"".as_slice(),
            b"uR\"".as_slice(),
            b"UR\"".as_slice(),
            b"LR\"".as_slice(),
        ]
        .into_iter()
        .find_map(|prefix| source.strip_prefix(prefix))
        {
            source = after_raw_string(raw);
        } else if first.is_ascii_digit() {
            source = after_number(rest);
        } else if matches!(first, b'\'' | b'"') {
            source = after_quoted(rest, first);
        } else {
            if [b"@interface".as_slice(), b"@implementation".as_slice()]
                .iter()
                .any(|keyword| {
                    source.strip_prefix(*keyword).is_some_and(|suffix| {
                        suffix.first().is_none_or(|byte| {
                            !byte.is_ascii_alphanumeric() && *byte != b'_' && byte.is_ascii()
                        })
                    })
                })
                || source.starts_with(b"#import <Foundation/")
                || source.starts_with(b"#import \"")
            {
                return true;
            }
            source = rest;
        }
    }
    false
}

pub(super) fn after_number(mut source: &[u8]) -> &[u8] {
    while let Some((&first, rest)) = source.split_first() {
        // C++ digit separators are part of the number, not character literals.
        if first.is_ascii_alphanumeric()
            || matches!(first, b'_' | b'.')
            || (first == b'\'' && rest.first().is_some_and(u8::is_ascii_alphanumeric))
        {
            source = rest;
        } else {
            break;
        }
    }
    source
}

pub(super) fn after_line_comment(mut source: &[u8]) -> &[u8] {
    while let Some(newline) = source.iter().position(|byte| *byte == b'\n') {
        let line = source.get(..newline).unwrap_or_default();
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        source = source.get(newline + 1..).unwrap_or_default();
        // C-family line splicing precedes comments: the continued physical line
        // remains documentation even when it contains an apparent directive.
        if !line.ends_with(b"\\") {
            return source;
        }
    }
    &[]
}

pub(super) fn after_quoted(mut source: &[u8], delimiter: u8) -> &[u8] {
    while let Some((&first, rest)) = source.split_first() {
        if first == delimiter {
            return rest;
        }
        source = if first == b'\\' {
            rest.get(1..).unwrap_or_default()
        } else {
            rest
        };
    }
    &[]
}

fn after_raw_string(source: &[u8]) -> &[u8] {
    let Some(open) = source.iter().take(17).position(|byte| *byte == b'(') else {
        return &[];
    };
    let delimiter = source.get(..open).unwrap_or_default();
    if delimiter
        .iter()
        .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b')' | b'\\') || !byte.is_ascii())
    {
        return &[];
    }
    let body = source.get(open + 1..).unwrap_or_default();
    // The C++ raw delimiter is at most 16 bytes. Matching its exact suffix
    // avoids treating quotes inside examples as the end of the literal.
    let closing_length = delimiter.len() + 2;
    for (offset, window) in body.windows(closing_length).enumerate() {
        if window.first() == Some(&b')')
            && window.last() == Some(&b'"')
            && window.get(1..closing_length - 1) == Some(delimiter)
        {
            return body.get(offset + closing_length..).unwrap_or_default();
        }
    }
    &[]
}
