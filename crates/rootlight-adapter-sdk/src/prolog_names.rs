//! Source-backed identity for Prolog predicate and DCG heads with static atoms.
//! The parser and structural lowering share this boundary so a captured head
//! cannot acquire a different arity or indicator kind during normalization.

/// Returns the written atom and arity of an unqualified clause head.
///
/// Quoted names without escapes retain their source spelling. Operators and
/// module-qualified heads remain unavailable without syntax and scope evidence.
#[must_use]
pub fn predicate_head(text: &str) -> Option<(&str, usize)> {
    let head = text.trim();
    let name_end = if head.starts_with('\'') {
        let closing = head.get(1..)?.find('\'')?;
        let inner = head.get(1..closing + 1)?;
        // Backslash escapes depend on a mutable Prolog flag, so source text
        // alone cannot assign their semantic atom identity safely.
        if inner.is_empty() || inner.contains('\\') || inner.chars().any(char::is_control) {
            return None;
        }
        closing + 2
    } else {
        unquoted_name_end(head)?
    };
    let name = head.get(..name_end)?;
    let rest = head.get(name_end..)?.trim();
    if rest.is_empty() {
        return Some((name, 0));
    }
    let args = rest.strip_prefix('(')?.strip_suffix(')')?;
    let arity = argument_count(args)?;
    Some((name, arity))
}

fn unquoted_name_end(head: &str) -> Option<usize> {
    let first = head.chars().next()?;
    if !first.is_lowercase() && !(first.is_alphabetic() && !first.is_uppercase()) {
        return None;
    }
    head.char_indices()
        .take_while(|(_, ch)| ch.is_alphanumeric() || *ch == '_')
        .last()
        .map(|(index, ch)| index + ch.len_utf8())
}

fn canonical_atom_spelling(name: &str) -> &str {
    let Some(inner) = name
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    else {
        return name;
    };
    if unquoted_name_end(inner) == Some(inner.len()) {
        inner
    } else {
        name
    }
}

/// Returns the byte length of a `0'c` character-code literal from its quote.
///
/// This recognizes the lexical boundary only; escaped code values are not
/// interpreted because their meaning can depend on Prolog flags.
#[must_use]
pub fn character_code_literal_len(source: &[u8], quote_index: usize) -> Option<usize> {
    if source.get(quote_index) != Some(&b'\'')
        || source.get(quote_index.checked_sub(1)?) != Some(&b'0')
        || quote_index >= 2
            && source
                .get(quote_index - 2)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte >= 0x80)
    {
        return None;
    }
    let mut value_index = quote_index.checked_add(1)?;
    if source.get(value_index) == Some(&b'\\') {
        value_index = value_index.checked_add(1)?;
    }
    let first = *source.get(value_index)?;
    let width = match first {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return None,
    };
    let end = value_index.checked_add(width)?;
    if !source
        .get(value_index + 1..end)?
        .iter()
        .all(|byte| (0x80..=0xbf).contains(byte))
    {
        return None;
    }
    end.checked_sub(quote_index)
}

/// Returns the canonical predicate indicator from a source-backed clause head.
#[must_use]
pub fn canonical_predicate_head(text: &str, maximum_bytes: usize) -> Option<String> {
    let (name, arity) = predicate_head(text)?;
    let name = canonical_atom_spelling(name);
    let indicator = format!("{name}/{arity}");
    (indicator.len() <= maximum_bytes).then_some(indicator)
}

/// Returns the canonical DCG nonterminal indicator from a source-backed head.
///
/// The `//` identity is distinct from the ordinary predicate's `/` identity;
/// no assumption is made about a particular Prolog engine's DCG expansion.
#[must_use]
pub fn canonical_nonterminal_head(text: &str, maximum_bytes: usize) -> Option<String> {
    let (name, arity) = predicate_head(text)?;
    let name = canonical_atom_spelling(name);
    let indicator = format!("{name}//{arity}");
    (indicator.len() <= maximum_bytes).then_some(indicator)
}

fn argument_count(args: &str) -> Option<usize> {
    let bytes = args.as_bytes();
    let mut parens = 0_usize;
    let mut brackets = 0_usize;
    let mut braces = 0_usize;
    let mut quote = None;
    let mut segment_has_code = false;
    let mut arity = 1_usize;
    let mut index = 0_usize;
    while index < bytes.len() {
        let byte = *bytes.get(index)?;
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                index = index.checked_add(2)?;
                continue;
            }
            if byte == delimiter {
                if bytes.get(index + 1) == Some(&delimiter) {
                    index += 2;
                    continue;
                }
                quote = None;
            }
        } else if byte == b'%' {
            index = bytes[index..]
                .iter()
                .position(|candidate| *candidate == b'\n')
                .map_or(bytes.len(), |offset| index + offset);
            continue;
        } else if bytes[index..].starts_with(b"/*") {
            let end = bytes[index + 2..]
                .windows(2)
                .position(|pair| pair == b"*/")?;
            index += end + 4;
            continue;
        } else if byte == b'\''
            && let Some(length) = character_code_literal_len(bytes, index)
        {
            segment_has_code = true;
            index = index.checked_add(length)?;
            continue;
        } else {
            match byte {
                b'\'' | b'"' | b'`' => {
                    quote = Some(byte);
                    segment_has_code = true;
                }
                b'(' => {
                    parens = parens.checked_add(1)?;
                    segment_has_code = true;
                }
                b')' => parens = parens.checked_sub(1)?,
                b'[' => {
                    brackets = brackets.checked_add(1)?;
                    segment_has_code = true;
                }
                b']' => brackets = brackets.checked_sub(1)?,
                b'{' => {
                    braces = braces.checked_add(1)?;
                    segment_has_code = true;
                }
                b'}' => braces = braces.checked_sub(1)?,
                b',' if parens == 0 && brackets == 0 && braces == 0 => {
                    if !segment_has_code {
                        return None;
                    }
                    arity = arity.checked_add(1)?;
                    segment_has_code = false;
                }
                _ if !byte.is_ascii_whitespace() => segment_has_code = true,
                _ => {}
            }
        }
        index += 1;
    }
    (quote.is_none() && parens == 0 && brackets == 0 && braces == 0 && segment_has_code)
        .then_some(arity)
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_nonterminal_head, canonical_predicate_head, character_code_literal_len,
        predicate_head,
    };

    #[test]
    fn clause_head_identity_keeps_arity_and_exact_written_atom() {
        for (source, expected) in [
            ("edge", ("edge", 0)),
            ("edge(A, B)", ("edge", 2)),
            ("'edge'(A, B)", ("'edge'", 2)),
            ("'<urn:edge>'(A, B)", ("'<urn:edge>'", 2)),
            ("token(0'@, Input, Next)", ("token", 3)),
            ("pair(0'(, 0',)", ("pair", 2)),
            ("route([A,B], pair(1, 2), 'a,b')", ("route", 3)),
            ("λ(1)", ("λ", 1)),
            ("edge(A, /* comma, ignored */ B)", ("edge", 2)),
        ] {
            assert_eq!(predicate_head(source), Some(expected), "{source}");
        }
        assert_eq!(
            canonical_predicate_head("edge(A, B)", 6),
            Some("edge/2".to_owned())
        );
        assert_eq!(canonical_predicate_head("edge(A, B)", 5), None);
        assert_eq!(
            canonical_nonterminal_head("edge(A, B)", 7),
            Some("edge//2".to_owned())
        );
        assert_eq!(canonical_nonterminal_head("edge(A, B)", 6), None);
        assert_eq!(
            canonical_predicate_head("'edge'(A, B)", 6),
            Some("edge/2".to_owned())
        );
        assert_eq!(
            canonical_predicate_head("'<urn:edge>'(A, B)", 14),
            Some("'<urn:edge>'/2".to_owned())
        );
    }

    #[test]
    fn character_code_literal_boundary_preserves_delimiters_as_data() {
        for (source, quote, length) in
            [("0'@", 1, 2), ("0'(", 1, 2), ("0'λ", 1, 3), ("0'\\n", 1, 3)]
        {
            assert_eq!(
                character_code_literal_len(source.as_bytes(), quote),
                Some(length)
            );
        }
        for source in ["10'@", "name0'@", "0'"] {
            let quote = source.find('\'').expect("fixture has a quote");
            assert_eq!(character_code_literal_len(source.as_bytes(), quote), None);
        }
    }

    #[test]
    fn unsupported_or_malformed_heads_do_not_gain_identity() {
        for source in [
            "Edge(A)",
            "module:edge(A)",
            "'escaped\\'name'(A)",
            "'doubled''quote'(A)",
            "edge()",
            "edge(A,)",
            "edge(,A)",
            "edge((A)",
            "edge('unfinished)",
            "edge(A) :- body",
        ] {
            assert_eq!(predicate_head(source), None, "{source}");
        }
    }
}
