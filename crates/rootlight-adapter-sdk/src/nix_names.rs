//! Static Nix attribute names shared by lexical binding and search display.
//! Decoding never evaluates expressions; display paths preserve segment boundaries
//! while durable canonical names and all source evidence remain authored spelling.

use std::borrow::Cow;

use rootlight_cancel::Cancellation;

use crate::AdapterError;

/// Decodes one bounded static Nix attribute name without evaluating expressions.
///
/// Bare identifiers are borrowed. Quoted strings follow Nix escape and newline
/// rules, including literal dollar pairs. Empty and nonidentifier keys are valid;
/// paths, interpolation, malformed names, excess bytes or allocation failure
/// return `None`. Both input and output fit `maximum_bytes`.
///
/// # Errors
/// Returns [`AdapterError::Cancelled`] when cancellation interrupts decoding.
pub fn nix_static_attribute_name<'a>(
    written: &'a str,
    maximum_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Option<Cow<'a, str>>, AdapterError> {
    decode_name(written, maximum_bytes, || Ok(cancellation.check()?))
}

fn bare_identifier(text: &str) -> bool {
    text.as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && text.bytes().all(identifier_continuation)
}

/// Canonicalizes one static Nix key for an implicit attribute-path owner.
///
/// Unlike display formatting, this always preserves a complete key syntax:
/// nonidentifiers remain quoted and equivalent escapes share one spelling.
/// Input and output must fit `maximum_bytes`; malformed, evaluated or
/// unrepresentable keys return `None`. Authored source spans are not changed.
///
/// # Errors
/// Returns [`AdapterError::Cancelled`] when decoding is interrupted.
pub fn nix_canonical_attribute_name<'a>(
    written: &'a str,
    maximum_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Option<Cow<'a, str>>, AdapterError> {
    let Some(name) = nix_static_attribute_name(written, maximum_bytes, cancellation)? else {
        return Ok(None);
    };
    if bare_identifier(&name) {
        return Ok(Some(name));
    }
    let mut output = String::new();
    Ok(append_segment(&mut output, &name, maximum_bytes).map(|()| Cow::Owned(output)))
}

fn identifier_continuation(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'\'')
}

fn decode_name<'a>(
    written: &'a str,
    maximum_bytes: usize,
    mut check: impl FnMut() -> Result<(), AdapterError>,
) -> Result<Option<Cow<'a, str>>, AdapterError> {
    check()?;
    if written.len() > maximum_bytes {
        return Ok(None);
    }
    if bare_identifier(written) {
        return Ok(Some(Cow::Borrowed(written)));
    }
    let Some(inner) = written
        .strip_prefix('"')
        .and_then(|name| name.strip_suffix('"'))
    else {
        return Ok(None);
    };
    let mut decoded = inner.contains(['\\', '\r']).then(String::new);
    let mut chars = inner.chars().peekable();
    while let Some(character) = chars.next() {
        check()?;
        let character = match character {
            '\0' | '"' => return Ok(None),
            '$' if chars.peek() == Some(&'{') => return Ok(None),
            '$' if chars.peek() == Some(&'$') => {
                // Nix consumes dollar pairs as literal content: $${ is static,
                // whereas $$${ contains an interpolation after the first pair.
                chars.next();
                if let Some(decoded) = &mut decoded
                    && append(decoded, "$", maximum_bytes).is_none()
                {
                    return Ok(None);
                }
                '$'
            }
            // Nix lexer.l unescapeStr is not JSON: unknown escapes drop the
            // backslash, and only unescaped CR/CRLF normalize to LF.
            '\\' => match chars.next() {
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('\0') | None => return Ok(None),
                Some(escaped) => escaped,
            },
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                '\n'
            }
            character => character,
        };
        if let Some(decoded) = &mut decoded {
            let mut bytes = [0; 4];
            if append(decoded, character.encode_utf8(&mut bytes), maximum_bytes).is_none() {
                return Ok(None);
            }
        }
    }
    Ok(Some(decoded.map_or(Cow::Borrowed(inner), Cow::Owned)))
}

pub(crate) fn display_path(text: &str) -> Option<Cow<'_, str>> {
    if text.contains('\0') {
        return None;
    }
    if bare_identifier(text) {
        return Some(Cow::Borrowed(text));
    }
    let (first, rest) = segment(skip_trivia(text)?)?;
    let rest = skip_trivia(rest)?;
    if rest.is_empty() && readable_single_name(&first) {
        return Some(first);
    }
    let mut output = String::new();
    append_segment(&mut output, &first, text.len())?;
    let mut rest = rest;
    while !rest.is_empty() {
        rest = skip_trivia(rest.strip_prefix('.')?)?;
        let (name, tail) = segment(rest)?;
        append(&mut output, ".", text.len())?;
        append_segment(&mut output, &name, text.len())?;
        rest = skip_trivia(tail)?;
    }
    if output == text {
        Some(Cow::Borrowed(text))
    } else {
        Some(Cow::Owned(output))
    }
}

fn readable_single_name(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && !name.chars().any(char::is_control)
        && !name.contains(['.', '"', '\\'])
        && !name.contains("${")
}

fn segment(text: &str) -> Option<(Cow<'_, str>, &str)> {
    let end = if text.starts_with('"') {
        let mut chars = text.char_indices();
        chars.next();
        loop {
            let (offset, character) = chars.next()?;
            if character == '\\' {
                chars.next()?;
            } else if character == '"' {
                break offset.checked_add(1)?;
            }
        }
    } else {
        text.bytes()
            .take_while(|byte| identifier_continuation(*byte))
            .count()
    };
    let written = text.get(..end)?;
    let decoded = decode_name(written, text.len(), || Ok(())).ok()??;
    Some((decoded, text.get(end..)?))
}

fn skip_trivia(mut text: &str) -> Option<&str> {
    loop {
        text = text.trim_start_matches([' ', '\t', '\r', '\n']);
        if let Some(comment) = text.strip_prefix("/*") {
            text = comment.get(comment.find("*/")?.checked_add(2)?..)?;
        } else if let Some(comment) = text.strip_prefix('#') {
            text = comment
                .find('\n')
                .and_then(|end| comment.get(end..))
                .unwrap_or("");
        } else {
            return Some(text);
        }
    }
}

fn append_segment(output: &mut String, name: &str, maximum_bytes: usize) -> Option<()> {
    if bare_identifier(name) {
        return append(output, name, maximum_bytes);
    }
    append(output, "\"", maximum_bytes)?;
    let mut chars = name.chars().peekable();
    while let Some(character) = chars.next() {
        let mut bytes = [0; 4];
        let encoded = match character {
            '\\' => "\\\\",
            '"' => "\\\"",
            '\n' => "\\n",
            '\r' => "\\r",
            '\t' => "\\t",
            '$' if chars.peek() == Some(&'{') => "\\$",
            character if character.is_control() => return None,
            character => character.encode_utf8(&mut bytes),
        };
        append(output, encoded, maximum_bytes)?;
    }
    append(output, "\"", maximum_bytes)
}

fn append(output: &mut String, text: &str, maximum_bytes: usize) -> Option<()> {
    if output.len().checked_add(text.len())? > maximum_bytes {
        return None;
    }
    output.try_reserve(text.len()).ok()?;
    output.push_str(text);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_key_identity_is_canonical_bounded_and_not_a_display_label() {
        for (written, expected) in [
            (r#""\a""#, "a"),
            (r#""a.b""#, r#""a.b""#),
            (r#""λ😀""#, r#""λ😀""#),
            (r#""""#, r#""""#),
            ("\"\r\"", r#""\n""#),
        ] {
            let canonical = nix_canonical_attribute_name(written, 64, &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(canonical, expected);
            assert_eq!(
                nix_static_attribute_name(&canonical, 64, &Cancellation::new()).unwrap(),
                nix_static_attribute_name(written, 64, &Cancellation::new()).unwrap()
            );
            assert_eq!(
                nix_canonical_attribute_name(&canonical, 64, &Cancellation::new())
                    .unwrap()
                    .as_deref(),
                Some(canonical.as_ref())
            );
        }
        assert!(
            nix_canonical_attribute_name("\"\r\"", 3, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(
            nix_canonical_attribute_name(r#""${value}""#, 64, &Cancellation::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn literal_dollar_pairs_and_escaped_dollars_remain_distinct_from_interpolation() {
        for (written, expected) in [
            (r#""$${literal}""#, Some("$${literal}")),
            (r#""\x$${literal}""#, Some("x$${literal}")),
            (r#""$$$$""#, Some("$$$$")),
            (r#""$$${value}""#, None),
            (r#""\$${value}""#, None),
            (r#""\$\${literal}""#, Some("$${literal}")),
        ] {
            assert_eq!(
                nix_static_attribute_name(written, 64, &Cancellation::new())
                    .unwrap()
                    .as_deref(),
                expected
            );
        }
    }

    #[test]
    fn malformed_paths_and_unreadable_expansion_never_replace_authored_display() {
        for text in [
            "a..b",
            "a.",
            ".a",
            "a b",
            "a/*",
            r#"a.${key}"#,
            r#""${key}""#,
            "123",
            "\"unclosed",
            "\"a\0b\"",
            "\"\r\"",
            "\"\u{1b}\"",
        ] {
            assert!(display_path(text).is_none(), "{text:?}");
        }
        for source in [r#""""#, r#"" a ""#, r#""\n""#, r#""a.b""#] {
            let display = display_path(source).unwrap();
            assert!(!display.is_empty());
            assert!(display.len() <= source.len());
        }
    }

    #[test]
    fn path_display_keeps_segment_boundaries_and_never_expands_or_changes_identity() {
        for source in ["a.b", r#""\a"."b""#, "a /* note */ . b", "a # note\n .b"] {
            assert_eq!(display_path(source).as_deref(), Some("a.b"));
        }
        assert_eq!(display_path(r#""a.b""#).as_deref(), Some(r#""a.b""#));
        assert_eq!(display_path("\"\r\n\"").as_deref(), Some(r#""\n""#));
        for name in [
            "",
            "x",
            "λ😀",
            "a.b",
            " a ",
            "two words",
            "${value}",
            "$${value}",
            "\\",
            "\"",
            "\n",
            "\r",
            "\t",
        ] {
            let mut quoted = String::new();
            append_segment(&mut quoted, name, 256).unwrap();
            assert_eq!(
                nix_static_attribute_name(&quoted, 256, &Cancellation::new())
                    .unwrap()
                    .as_deref(),
                Some(name)
            );
            for source in [&quoted, &format!("a.{quoted}")] {
                let shown = crate::structural_display_name_for_language("nix", source);
                assert!(!shown.is_empty());
                assert!(shown.len() <= source.len());
                assert_eq!(
                    crate::structural_display_name_for_language("nix", &shown),
                    shown
                );
                assert_eq!(
                    crate::structural_captured_name_for_language("nix", source, source.len())
                        .as_deref(),
                    Some(source.as_str())
                );
            }
        }
    }
}
