//! Resolve URI-fragment JSON Pointers against exact native JSON member positions.
//! Schema dialect, resource ownership and target declaration admission remain
//! separate caller proofs; a located JSON value alone establishes no UsesType.

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::{Cancellation, Cancelled};
use tree_sitter::Node;

#[derive(Debug)]
pub(crate) enum PointerError {
    Barrier(&'static str),
    Adapter(AdapterError),
}

impl From<AdapterError> for PointerError {
    fn from(error: AdapterError) -> Self {
        Self::Adapter(error)
    }
}

impl From<Cancelled> for PointerError {
    fn from(error: Cancelled) -> Self {
        Self::Adapter(error.into())
    }
}

pub(crate) struct Work<'a> {
    remaining: usize,
    maximum_string_bytes: usize,
    cancellation: &'a Cancellation,
}

impl<'a> Work<'a> {
    pub(crate) const fn new(
        remaining: usize,
        maximum_string_bytes: usize,
        cancellation: &'a Cancellation,
    ) -> Self {
        Self {
            remaining,
            maximum_string_bytes,
            cancellation,
        }
    }

    pub(crate) fn step(&mut self) -> Result<(), PointerError> {
        self.cancellation.check()?;
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or(PointerError::Barrier("json-schema-pointer-work-limit"))?;
        Ok(())
    }

    fn output(&mut self, maximum: usize) -> Result<String, PointerError> {
        self.step()?;
        if maximum > self.maximum_string_bytes {
            return Err(PointerError::Barrier("json-schema-pointer-string-limit"));
        }
        let mut result = String::new();
        result
            .try_reserve(maximum)
            .map_err(|_| AdapterError::from(SinkError::AllocationFailed))?;
        Ok(result)
    }
}

fn barrier<T>(reason: &'static str) -> Result<T, PointerError> {
    Err(PointerError::Barrier(reason))
}

fn hex(character: char) -> Result<u16, PointerError> {
    character
        .to_digit(16)
        .and_then(|digit| u16::try_from(digit).ok())
        .ok_or(PointerError::Barrier("json-schema-invalid-json-string"))
}

fn unit(input: &mut std::str::Chars<'_>, work: &mut Work<'_>) -> Result<u16, PointerError> {
    let mut result = 0u16;
    for _ in 0..4 {
        work.step()?;
        let next = input
            .next()
            .ok_or(PointerError::Barrier("json-schema-invalid-json-string"))?;
        result = result
            .checked_mul(16)
            .and_then(|value| value.checked_add(hex(next).ok()?))
            .ok_or(PointerError::Barrier("json-schema-invalid-json-string"))?;
    }
    Ok(result)
}

/// Decode one complete JSON string without changing its separate physical span.
/// Unpaired surrogate units are an explicit unsupported-name barrier, not U+FFFD.
pub(crate) fn decode_string(raw: &str, work: &mut Work<'_>) -> Result<String, PointerError> {
    let body = raw
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .ok_or(PointerError::Barrier("json-schema-invalid-json-string"))?;
    let mut output = work.output(body.len())?;
    let mut input = body.chars();
    while let Some(character) = input.next() {
        work.step()?;
        if character == '"' || character < '\u{20}' {
            return barrier("json-schema-invalid-json-string");
        }
        if character != '\\' {
            output.push(character);
            continue;
        }
        work.step()?;
        match input.next() {
            Some('"') => output.push('"'),
            Some('\\') => output.push('\\'),
            Some('/') => output.push('/'),
            Some('b') => output.push('\u{8}'),
            Some('f') => output.push('\u{c}'),
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('u') => {
                let first = unit(&mut input, work)?;
                let scalar = if (0xd800..=0xdbff).contains(&first) {
                    work.step()?;
                    if input.next() != Some('\\') || input.next() != Some('u') {
                        return barrier("json-schema-unpaired-surrogate-name");
                    }
                    let low = unit(&mut input, work)?;
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return barrier("json-schema-unpaired-surrogate-name");
                    }
                    0x10000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(low) - 0xdc00)
                } else {
                    u32::from(first)
                };
                output.push(
                    char::from_u32(scalar)
                        .ok_or(PointerError::Barrier("json-schema-unpaired-surrogate-name"))?,
                );
            }
            _ => return barrier("json-schema-invalid-json-string"),
        }
    }
    Ok(output)
}

fn fragment_character(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'/'
                | b'?'
        )
}

fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn subdelimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
    )
}

fn uri_component(text: &str, allowed: impl Fn(u8) -> bool) -> bool {
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            if !bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit())
                || !bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit())
            {
                return false;
            }
        } else if !allowed(byte) {
            return false;
        }
    }
    true
}

/// Check the RFC 3986 absolute-URI syntax used by the root resource base.
/// This does not dereference a URI, normalize it or validate scheme-specific rules.
pub(crate) fn absolute_uri(text: &str) -> bool {
    if !text.is_ascii() || text.contains('#') {
        return false;
    }
    let Some((scheme, rest)) = text.split_once(':') else {
        return false;
    };
    if !scheme
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic())
        || !scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    {
        return false;
    }
    let (hierarchy, query) = rest
        .split_once('?')
        .map_or((rest, None), |(hierarchy, query)| (hierarchy, Some(query)));
    if query.is_some_and(|query| !uri_component(query, fragment_character)) {
        return false;
    }
    let pchar = |byte| unreserved(byte) || subdelimiter(byte) || matches!(byte, b':' | b'@');
    let Some(authority_path) = hierarchy.strip_prefix("//") else {
        return uri_component(hierarchy, |byte| pchar(byte) || byte == b'/');
    };
    let (authority, path) = authority_path
        .split_once('/')
        .map_or((authority_path, ""), |(authority, path)| (authority, path));
    if !uri_component(path, |byte| pchar(byte) || byte == b'/') {
        return false;
    }
    let host_port = if let Some((userinfo, host)) = authority.rsplit_once('@') {
        if !uri_component(userinfo, |byte| {
            unreserved(byte) || subdelimiter(byte) || byte == b':'
        }) {
            return false;
        }
        host
    } else {
        authority
    };
    if let Some(literal) = host_port.strip_prefix('[') {
        let Some((address, port)) = literal.split_once(']') else {
            return false;
        };
        if !port.is_empty()
            && !port
                .strip_prefix(':')
                .is_some_and(|port| port.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return false;
        }
        if address.parse::<std::net::Ipv6Addr>().is_ok() {
            return true;
        }
        let Some(future) = address
            .strip_prefix('v')
            .or_else(|| address.strip_prefix('V'))
        else {
            return false;
        };
        let Some((version, value)) = future.split_once('.') else {
            return false;
        };
        return !version.is_empty()
            && version.bytes().all(|byte| byte.is_ascii_hexdigit())
            && !value.is_empty()
            && value
                .bytes()
                .all(|byte| unreserved(byte) || subdelimiter(byte) || byte == b':');
    }
    let host = if let Some((host, port)) = host_port.rsplit_once(':') {
        if !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        host
    } else {
        host_port
    };
    uri_component(host, |byte| unreserved(byte) || subdelimiter(byte))
}

/// Decode a fragment-only URI reference into exact Unicode pointer tokens.
/// JSON unescaping precedes percent decoding, which precedes RFC 6901 decoding.
pub(crate) fn fragment_tokens(
    raw_json_string: &str,
    work: &mut Work<'_>,
) -> Result<Vec<String>, PointerError> {
    let reference = decode_string(raw_json_string, work)?;
    let fragment = reference.strip_prefix('#').ok_or(PointerError::Barrier(
        "json-schema-external-reference-unavailable",
    ))?;
    let mut decoded = Vec::new();
    decoded
        .try_reserve(fragment.len())
        .map_err(|_| AdapterError::from(SinkError::AllocationFailed))?;
    let mut bytes = fragment.bytes();
    while let Some(byte) = bytes.next() {
        work.step()?;
        if byte == b'%' {
            let high = bytes
                .next()
                .map(char::from)
                .map(hex)
                .transpose()?
                .ok_or(PointerError::Barrier("json-schema-invalid-uri-fragment"))?;
            let low = bytes
                .next()
                .map(char::from)
                .map(hex)
                .transpose()?
                .ok_or(PointerError::Barrier("json-schema-invalid-uri-fragment"))?;
            decoded.push(
                u8::try_from(high * 16 + low)
                    .map_err(|_| PointerError::Barrier("json-schema-invalid-uri-fragment"))?,
            );
        } else if fragment_character(byte) {
            decoded.push(byte);
        } else {
            return barrier("json-schema-invalid-uri-fragment");
        }
    }
    let decoded = std::str::from_utf8(&decoded)
        .map_err(|_| PointerError::Barrier("json-schema-invalid-uri-fragment"))?;
    if decoded.is_empty() {
        return Ok(Vec::new());
    }
    let body = decoded.strip_prefix('/').ok_or(PointerError::Barrier(
        "json-schema-anchor-reference-unavailable",
    ))?;
    let mut tokens = Vec::new();
    for raw in body.split('/') {
        let mut token = work.output(raw.len())?;
        let mut chars = raw.chars();
        while let Some(character) = chars.next() {
            work.step()?;
            if character == '~' {
                match chars.next() {
                    Some('0') => token.push('~'),
                    Some('1') => token.push('/'),
                    _ => return barrier("json-schema-invalid-json-pointer"),
                }
            } else {
                token.push(character);
            }
        }
        tokens
            .try_reserve(1)
            .map_err(|_| AdapterError::from(SinkError::AllocationFailed))?;
        tokens.push(token);
    }
    Ok(tokens)
}

/// Locate a physical JSON value without asserting schema or entity ownership.
/// Duplicate decoded member names and noncanonical array indexes fail closed.
pub(crate) fn resolve_native<'tree>(
    mut value: Node<'tree>,
    tokens: &[String],
    source: &[u8],
    work: &mut Work<'_>,
) -> Result<Node<'tree>, PointerError> {
    for token in tokens {
        work.step()?;
        if value.has_error() || value.is_missing() {
            return barrier("json-schema-recovered-pointer-path");
        }
        value = match value.kind() {
            "object" => {
                let mut found = None;
                let mut cursor = value.walk();
                for member in value.named_children(&mut cursor) {
                    work.step()?;
                    if member.kind() != "pair" || member.has_error() || member.is_missing() {
                        return barrier("json-schema-invalid-pointer-path");
                    }
                    let key = member
                        .child_by_field_name("key")
                        .ok_or(PointerError::Barrier("json-schema-invalid-pointer-path"))?;
                    let raw = source
                        .get(key.byte_range())
                        .and_then(|raw| std::str::from_utf8(raw).ok())
                        .ok_or(PointerError::Barrier("json-schema-invalid-pointer-path"))?;
                    if decode_string(raw, work)? == *token {
                        if found.is_some() {
                            return barrier("json-schema-ambiguous-pointer-path");
                        }
                        found = member.child_by_field_name("value");
                    }
                }
                found.ok_or(PointerError::Barrier("json-schema-missing-pointer-target"))?
            }
            "array" => {
                if token.is_empty()
                    || (token.starts_with('0') && token != "0")
                    || !token.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return barrier("json-schema-invalid-array-index");
                }
                let mut index = 0usize;
                for digit in token.bytes() {
                    work.step()?;
                    index = index
                        .checked_mul(10)
                        .and_then(|index| index.checked_add(usize::from(digit - b'0')))
                        .ok_or(PointerError::Barrier("json-schema-invalid-array-index"))?;
                }
                let mut cursor = value.walk();
                let mut selected = None;
                for (position, child) in value.named_children(&mut cursor).enumerate() {
                    work.step()?;
                    if child.kind() == "comment" || child.has_error() || child.is_missing() {
                        return barrier("json-schema-invalid-pointer-path");
                    }
                    if position == index {
                        selected = Some(child);
                    }
                }
                selected.ok_or(PointerError::Barrier("json-schema-missing-pointer-target"))?
            }
            _ => return barrier("json-schema-scalar-pointer-path"),
        };
    }
    if value.has_error() || value.is_missing() {
        return barrier("json-schema-recovered-pointer-path");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{GrammarFamily, language_for};

    fn tokens(raw: &str) -> Result<Vec<String>, PointerError> {
        fragment_tokens(raw, &mut Work::new(20_000, 16_384, &Cancellation::new()))
    }

    fn located(source: &str, reference: &str) -> Result<std::ops::Range<usize>, PointerError> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Json))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node().named_child(0).unwrap();
        let cancel = Cancellation::new();
        let mut work = Work::new(20_000, 16_384, &cancel);
        let tokens = fragment_tokens(reference, &mut work)?;
        Ok(resolve_native(root, &tokens, source.as_bytes(), &mut work)?.byte_range())
    }

    #[test]
    fn json_percent_and_pointer_decoding_have_distinct_ordered_steps() {
        assert_eq!(
            tokens(r##""#/$defs/a~1b~0c""##).unwrap(),
            ["$defs", "a/b~c"]
        );
        assert_eq!(tokens(r##""#/~01""##).unwrap(), ["~1"]);
        assert_eq!(tokens(r##""#/%7E1""##).unwrap(), ["/"]);
        assert_eq!(tokens(r##""#/%CE%BB""##).unwrap(), ["λ"]);
        assert_eq!(tokens(r##""#/%252F""##).unwrap(), ["%2F"]);
        assert_eq!(tokens(r##""#\/a""##).unwrap(), ["a"]);
        assert!(tokens(r##""#""##).unwrap().is_empty());
        assert_eq!(tokens(r##""#/""##).unwrap(), [""]);
        for invalid in [
            r##""#/a~2""##,
            r##""#/a~""##,
            r##""#/%FF""##,
            r##""#/%0""##,
            r##""#/a b""##,
            r##""#anchor""##,
            r##""other.json#/a""##,
        ] {
            assert!(tokens(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn resource_bases_require_absolute_uri_syntax_without_fragments() {
        for uri in [
            "https://json-schema.org/tests/test-schema",
            "urn:example:schema",
            "https://example.invalid/a%20b?x=y",
            "https://[2001:db8::1]/schema",
            "custom://user:pass@example.invalid:8080/source",
            "file:///schema.json",
        ] {
            assert!(absolute_uri(uri), "{uri}");
        }
        for uri in [
            "relative",
            "../schema",
            "#fragment",
            "https://example.invalid/schema#x",
            "https://example.invalid/%ZZ",
            "https://example.invalid/a b",
            "https://[invalid]/schema",
            "1bad:source",
            "https://example.invalid:word/schema",
        ] {
            assert!(!absolute_uri(uri), "{uri}");
        }
    }

    #[test]
    fn decoded_duplicates_fail_instead_of_selecting_first_or_last() {
        let source = r#"{"model":{},"mo\u0064el":{}}"#;
        assert!(matches!(
            located(source, r##""#/model""##),
            Err(PointerError::Barrier("json-schema-ambiguous-pointer-path"))
        ));
        let source = r#"{"same":{"x":1},"other":{"x":2}}"#;
        let range = located(source, r##""#/other/x""##).unwrap();
        assert_eq!(&source[range], "2");
    }

    #[test]
    fn arrays_count_scalars_and_reject_noncanonical_or_overflowing_indices() {
        let source = r#"[true,false,null,7,"text",{"model":{}}]"#;
        let range = located(source, r##""#/5/model""##).unwrap();
        assert_eq!(&source[range], "{}");
        for reference in [
            r##""#/05/model""##,
            r##""#/-""##,
            r##""#/6""##,
            r##""#/99999999999999999999999999999""##,
        ] {
            assert!(located(source, reference).is_err(), "{reference}");
        }
    }

    #[test]
    fn pointer_unicode_is_not_normalized_and_original_ranges_are_preserved() {
        let source = r#"{"a/b~c":{"caf\u00e9":1},"é":2,"é":3}"#;
        let range = located(source, r##""#/a~1b~0c/caf%C3%A9""##).unwrap();
        assert_eq!(&source[range], "1");
        let decomposed = located(source, r##""#/e%CC%81""##).unwrap();
        let composed = located(source, r##""#/%C3%A9""##).unwrap();
        assert_ne!(decomposed, composed);
    }

    #[test]
    fn zero_work_and_precancelled_inputs_do_not_publish_pointer_proof() {
        let cancel = Cancellation::new();
        assert!(matches!(
            fragment_tokens(r##""#/a""##, &mut Work::new(0, 100, &cancel)),
            Err(PointerError::Barrier("json-schema-pointer-work-limit"))
        ));
        assert!(matches!(
            fragment_tokens(r##""#/a""##, &mut Work::new(100, 1, &cancel)),
            Err(PointerError::Barrier("json-schema-pointer-string-limit"))
        ));
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            fragment_tokens(r##""#/a""##, &mut Work::new(100, 100, &cancel)),
            Err(PointerError::Adapter(_))
        ));
    }
}
