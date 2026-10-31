//! Borrowed identities for native-CST-certified Turtle terms and prefix bindings.
//! This subset preserves written IRI bytes and rejects unresolved relative or
//! escaped forms; it performs no URL normalization, inference or external loading.

/// A supported written term with its spelling borrowed from the current source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Term<'a> {
    /// An absolute IRI without its written angle brackets.
    Absolute(&'a str),
    /// An authored prefix and local name, including the empty default prefix.
    Prefixed { prefix: &'a str, local: &'a str },
}

/// Retains a supported absolute IRI or unescaped QName from a native term span.
/// Full PN character grammar remains the native parser's responsibility.
pub(crate) fn term(text: &str) -> Option<Term<'_>> {
    if let Some(iri) = text
        .strip_prefix('<')
        .and_then(|text| text.strip_suffix('>'))
    {
        return absolute_iri(iri).then_some(Term::Absolute(iri));
    }
    if text.chars().any(|character| {
        character.is_control()
            || character.is_whitespace()
            || matches!(
                character,
                '<' | '>'
                    | '"'
                    | '\''
                    | '\\'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | ';'
                    | ','
                    | '/'
                    | '?'
                    | '#'
                    | '@'
                    | '='
                    | '^'
                    | '`'
                    | '|'
            )
    }) || !valid_percent_escapes(text)
    {
        return None;
    }
    let (prefix, local) = text.split_once(':')?;
    Some(Term::Prefixed { prefix, local })
}

/// Reads one complete native prefix directive, preserving unknown rebinding.
/// A known prefix with a relative or escaped IRI returns `Some((prefix, None))`
/// so callers invalidate an older binding instead of reusing it accidentally.
pub(crate) fn prefix_binding(text: &str) -> Option<(&str, Option<&str>)> {
    let text = skip_layout(text);
    let (tail, dotted) = if let Some(tail) = text.strip_prefix("@prefix") {
        (tail, true)
    } else {
        let keyword = text.get(..6)?;
        if !keyword.eq_ignore_ascii_case("PREFIX") {
            return None;
        }
        (text.get(6..)?, false)
    };
    if !tail.starts_with([' ', '\t', '\r', '\n', '#']) {
        return None;
    }
    let tail = skip_layout(tail);
    let (prefix, tail) = tail.split_once(':')?;
    if prefix.chars().any(|character| {
        character.is_control()
            || character.is_whitespace()
            || matches!(character, '<' | '>' | '"' | '\'' | '\\' | '#' | '/' | '?')
    }) {
        return None;
    }
    let tail = skip_layout(tail).strip_prefix('<')?;
    let (iri, tail) = tail.split_once('>')?;
    let tail = skip_layout(tail);
    let complete = if dotted {
        tail.strip_prefix('.')
            .is_some_and(|tail| skip_layout(tail).is_empty())
    } else {
        tail.is_empty()
    };
    complete.then_some((prefix, absolute_iri(iri).then_some(iri)))
}

/// Expands only a written absolute term or a proved current file-local binding.
pub(crate) fn expanded<'a>(
    text: &'a str,
    bindings: &std::collections::HashMap<&str, Option<&'a str>>,
) -> Option<(&'a str, &'a str)> {
    match term(text)? {
        Term::Absolute(iri) => Some((iri, "")),
        Term::Prefixed { prefix, local } => Some((bindings.get(prefix).copied().flatten()?, local)),
    }
}

fn absolute_iri(iri: &str) -> bool {
    let Some((scheme, _)) = iri.split_once(':') else {
        return false;
    };
    scheme
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
        && !iri.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(
                    character,
                    '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\'
                )
        })
        && valid_percent_escapes(iri)
}

fn valid_percent_escapes(text: &str) -> bool {
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%'
            && !(bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit())
                && bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit()))
        {
            return false;
        }
    }
    true
}

fn skip_layout(mut text: &str) -> &str {
    loop {
        text = text.trim_start_matches([' ', '\t', '\r', '\n']);
        if !text.starts_with('#') {
            return text;
        }
        text = text
            .find(['\r', '\n'])
            .and_then(|end| text.get(end..))
            .unwrap_or("");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_preserve_iri_and_digit_leading_qname_spelling() {
        assert_eq!(
            term(":3DModel"),
            Some(Term::Prefixed {
                prefix: "",
                local: "3DModel"
            })
        );
        assert_eq!(
            term("schema:3DModel"),
            Some(Term::Prefixed {
                prefix: "schema",
                local: "3DModel"
            })
        );
        assert_eq!(
            term("ex:name:part"),
            Some(Term::Prefixed {
                prefix: "ex",
                local: "name:part"
            })
        );
        assert_eq!(
            term("<https://example.test/3DModel#part>"),
            Some(Term::Absolute("https://example.test/3DModel#part"))
        );
        assert_eq!(
            term("<urn:example:%2f>"),
            Some(Term::Absolute("urn:example:%2f"))
        );
        for text in [
            "<relative>",
            "<3bad:value>",
            "<https://example.test/\\u0041>",
            "ex:escaped\\~",
            "ex:has space",
            "ex:bad/name",
            "<https://example.test/%zz>",
        ] {
            assert_eq!(term(text), None, "{text}");
        }
    }

    #[test]
    fn prefix_directives_keep_default_and_unknown_rebindings() {
        assert_eq!(
            prefix_binding("@prefix : <https://schema.org/> ."),
            Some(("", Some("https://schema.org/")))
        );
        assert_eq!(
            prefix_binding("pReFiX schema: <https://schema.org/>"),
            Some(("schema", Some("https://schema.org/")))
        );
        assert_eq!(
            prefix_binding("@prefix ex: <relative> ."),
            Some(("ex", None))
        );
        assert_eq!(
            prefix_binding("@prefix ex: <https://example.test/\\u0041> ."),
            Some(("ex", None))
        );
        assert_eq!(
            prefix_binding(
                "@prefix # name\r\n ex: # value\n <https://example.test/#fragment> # terminator\n . # done"
            ),
            Some(("ex", Some("https://example.test/#fragment")))
        );
        for text in [
            "@prefix ex: <https://example.test/>",
            "PREFIXex: <https://example.test/>",
            "PREFIX ex: <https://example.test/> .",
            "@prefix ex: <https://example.test/> . junk",
        ] {
            assert_eq!(prefix_binding(text), None, "{text}");
        }
    }
}
