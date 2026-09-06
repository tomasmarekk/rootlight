//! Canonical static names from grammar-reviewed Lua definition captures.
//!
//! This helper preserves source spans separately from normalized
//! names. It is not a Lua parser or a replacement for receiver resolution.

#![forbid(unsafe_code)]

use std::borrow::Cow;

/// Normalizes a grammar-admitted identifier/member capture within a byte budget.
///
/// Compact names borrow their input. Formatting and Lua comments are removed
/// without evaluating receivers. Non-static forms, malformed trivia, excessive
/// source bytes and allocation failure return `None`; callers must expose that
/// missing identity rather than claim complete semantic coverage.
pub(crate) fn canonical_lua_name(text: &str, maximum_bytes: usize) -> Option<Cow<'_, str>> {
    if text.len() > maximum_bytes {
        return None;
    }
    let text = text.trim();
    if compact_name(text) {
        return Some(Cow::Borrowed(text));
    }
    let mut normalized = String::new();
    normalized.try_reserve_exact(text.len()).ok()?;
    let mut remaining = skip_trivia(text)?;
    let mut method = false;
    loop {
        let (identifier, rest) = take_identifier(remaining)?;
        normalized.push_str(identifier);
        remaining = skip_trivia(rest)?;
        if remaining.is_empty() {
            return Some(Cow::Owned(normalized));
        }
        if method {
            return None;
        }
        let separator = remaining.chars().next()?;
        if !matches!(separator, '.' | ':') {
            return None;
        }
        method = separator == ':';
        normalized.push(separator);
        remaining = skip_trivia(remaining.get(separator.len_utf8()..)?)?;
    }
}

fn compact_name(mut text: &str) -> bool {
    let mut method = false;
    loop {
        let Some((_, remaining)) = take_identifier(text) else {
            return false;
        };
        if remaining.is_empty() {
            return true;
        }
        if method {
            return false;
        }
        let Some(separator) = remaining.chars().next() else {
            return false;
        };
        if !matches!(separator, '.' | ':') {
            return false;
        }
        method = separator == ':';
        let Some(next) = remaining.get(separator.len_utf8()..) else {
            return false;
        };
        text = next;
    }
}

fn take_identifier(text: &str) -> Option<(&str, &str)> {
    let first = text.chars().next()?;
    if first.is_ascii_digit() || !identifier_character(first) {
        return None;
    }
    let end = text
        .char_indices()
        .find_map(|(offset, character)| (!identifier_character(character)).then_some(offset))
        .unwrap_or(text.len());
    Some((text.get(..end)?, text.get(end..)?))
}

fn identifier_character(character: char) -> bool {
    !character.is_control()
        && !character.is_whitespace()
        && !matches!(
            character,
            '+' | '-'
                | '*'
                | '/'
                | '%'
                | '^'
                | '#'
                | '&'
                | '~'
                | '|'
                | '<'
                | '>'
                | '='
                | '('
                | ')'
                | '{'
                | '}'
                | '['
                | ']'
                | ';'
                | ':'
                | ','
                | '.'
                | '\\'
                | '\''
                | '"'
        )
}

fn skip_trivia(mut text: &str) -> Option<&str> {
    loop {
        text = text.trim_start();
        let Some(comment) = text.strip_prefix("--") else {
            return Some(text);
        };
        if let Some(bracket) = comment.strip_prefix('[') {
            let equals = bracket.bytes().take_while(|byte| *byte == b'=').count();
            if bracket.as_bytes().get(equals) == Some(&b'[') {
                let mut closing = String::new();
                closing.try_reserve_exact(equals.checked_add(2)?).ok()?;
                closing.push(']');
                for _ in 0..equals {
                    closing.push('=');
                }
                closing.push(']');
                let body = bracket.get(equals.checked_add(1)?..)?;
                let end = body.find(closing.as_str())?;
                text = body.get(end.checked_add(closing.len())?..)?;
                continue;
            }
        }
        let end = comment.find(['\r', '\n']).unwrap_or(comment.len());
        text = comment.get(end..)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_qualified_names_borrow_without_losing_receivers() {
        for name in [
            "run",
            "M.run",
            "N.run",
            "M:run",
            "M.nested.run",
            "_ENV.print",
            "λ.μέλος",
        ] {
            assert_eq!(canonical_lua_name(name, 128), Some(Cow::Borrowed(name)));
            assert!(matches!(
                canonical_lua_name(name, 128),
                Some(Cow::Borrowed(_))
            ));
        }
        assert_eq!(
            canonical_lua_name(" M.run ", 128),
            Some(Cow::Borrowed("M.run"))
        );
    }

    #[test]
    fn formatting_and_comments_preserve_static_identity() {
        for (source, expected) in [
            ("M . nested : run", "M.nested:run"),
            ("M -- qualifier\n . run", "M.run"),
            ("M -- qualifier\r . run", "M.run"),
            ("M --[[a comment]] . run", "M.run"),
            ("M --[==[ignored ]=] and ]] markers]==] . run", "M.run"),
            ("-- leading\n M . run -- trailing", "M.run"),
            ("λ --[[unicode qualifier]] . μέλος", "λ.μέλος"),
        ] {
            let normalized = canonical_lua_name(source, 256).expect("static fixture normalizes");
            assert_eq!(normalized, expected);
            assert!(normalized.len() <= source.len());
            assert_eq!(
                canonical_lua_name(&normalized, 256).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn dynamic_receivers_and_malformed_captures_stay_unknown() {
        for source in [
            "",
            "123.run",
            "factory().run",
            "table['run']",
            "M..run",
            "M.",
            "M:run.more",
            "M:run:next",
            "M + run",
            "M / run",
            "M\0.run",
            "M --[=[unterminated . run",
            "M --[[wrong close]=] . run",
            "M --[[comment]] other",
            "-- only comment",
            "M /* foreign comment */ . run",
        ] {
            assert_eq!(canonical_lua_name(source, 256), None, "{source:?}");
        }
    }

    #[test]
    fn source_budget_is_exact_and_includes_trivia() {
        let source = "M --[[trivia]] . run";
        assert_eq!(
            canonical_lua_name(source, source.len()).as_deref(),
            Some("M.run")
        );
        assert_eq!(canonical_lua_name(source, source.len() - 1), None);
        assert_eq!(canonical_lua_name("M", 0), None);
        assert_eq!(
            canonical_lua_name("λ.run", "λ.run".len()).as_deref(),
            Some("λ.run")
        );
        assert_eq!(canonical_lua_name("λ.run", "λ.run".len() - 1), None);
    }
}
