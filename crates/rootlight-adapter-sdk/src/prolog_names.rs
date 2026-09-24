//! Source-backed identity for simple Prolog predicate and DCG heads.
//! The parser and structural lowering share this boundary so a captured head
//! cannot acquire a different arity or indicator kind during normalization.

/// Returns the written atom and arity of an unqualified clause head.
///
/// Quoted functor names, operators, and module-qualified heads are deliberately
/// unavailable until their syntax and scope can be represented without loss.
#[must_use]
pub fn predicate_head(text: &str) -> Option<(&str, usize)> {
    let head = text.trim();
    let first = head.chars().next()?;
    if !first.is_lowercase() && !(first.is_alphabetic() && !first.is_uppercase()) {
        return None;
    }
    let name_end = head
        .char_indices()
        .take_while(|(_, ch)| ch.is_alphanumeric() || *ch == '_')
        .last()
        .map(|(index, ch)| index + ch.len_utf8())?;
    let name = head.get(..name_end)?;
    let rest = head.get(name_end..)?.trim();
    if rest.is_empty() {
        return Some((name, 0));
    }
    let args = rest.strip_prefix('(')?.strip_suffix(')')?;
    let arity = argument_count(args)?;
    Some((name, arity))
}

/// Returns the canonical predicate indicator from a source-backed clause head.
#[must_use]
pub fn canonical_predicate_head(text: &str, maximum_bytes: usize) -> Option<String> {
    let (name, arity) = predicate_head(text)?;
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
    use super::{canonical_nonterminal_head, canonical_predicate_head, predicate_head};

    #[test]
    fn clause_head_identity_keeps_arity_and_exact_written_atom() {
        for (source, expected) in [
            ("edge", ("edge", 0)),
            ("edge(A, B)", ("edge", 2)),
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
    }

    #[test]
    fn unsupported_or_malformed_heads_do_not_gain_identity() {
        for source in [
            "Edge(A)",
            "module:edge(A)",
            "'quoted atom'(A)",
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
