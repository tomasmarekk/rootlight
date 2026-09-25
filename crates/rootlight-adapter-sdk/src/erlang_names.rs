//! Source-backed Erlang atom and function indicators.
//! Uninterpreted escapes keep their written spelling instead of claiming an
//! equivalence that requires compiler encoding or preprocessor context.

/// Returns the byte end of a written Erlang atom at the start of `text`.
#[must_use]
pub fn atom_end(text: &str) -> Option<usize> {
    if text.starts_with('\'') {
        let bytes = text.as_bytes();
        let mut cursor = 1;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'\\' => cursor = cursor.checked_add(2)?,
                b'\'' => return Some(cursor + 1),
                _ => cursor += 1,
            }
        }
        return None;
    }
    let mut characters = text.char_indices();
    let (_, first) = characters.next()?;
    if !first.is_lowercase() {
        return None;
    }
    let mut end = first.len_utf8();
    for (index, character) in characters {
        if character.is_alphanumeric() || matches!(character, '_' | '@') {
            end = index + character.len_utf8();
        } else {
            break;
        }
    }
    Some(end)
}

/// Canonicalizes only a statically written atom; escaped atoms stay distinct.
#[must_use]
pub(crate) fn canonical_atom(text: &str, maximum_bytes: usize) -> Option<String> {
    if atom_end(text) != Some(text.len()) || text.len() > maximum_bytes {
        return None;
    }
    let name = text
        .strip_prefix('\'')
        .and_then(|name| name.strip_suffix('\''))
        .filter(|name| !name.contains('\\') && atom_end(name) == Some(name.len()))
        .unwrap_or(text);
    Some(name.to_owned())
}

/// Returns the name and arity of an unqualified written function head or call.
#[must_use]
pub fn function_head(text: &str) -> Option<(&str, usize)> {
    let head = text.trim();
    let end = atom_end(head)?;
    let name = head.get(..end)?;
    let arguments = head.get(end..)?.trim();
    let arguments = arguments.strip_prefix('(')?.strip_suffix(')')?;
    Some((name, argument_count(arguments)?))
}

/// Returns the source-backed `name/arity` indicator of one unqualified call.
#[must_use]
pub fn canonical_function_head(text: &str, maximum_bytes: usize) -> Option<String> {
    let (name, arity) = function_head(text)?;
    let name = canonical_atom(name, maximum_bytes)?;
    let indicator = format!("{name}/{arity}");
    (indicator.len() <= maximum_bytes).then_some(indicator)
}

fn argument_count(arguments: &str) -> Option<usize> {
    let bytes = arguments.as_bytes();
    let mut cursor = 0;
    let mut parens = 0_usize;
    let mut brackets = 0_usize;
    let mut braces = 0_usize;
    let mut binaries = 0_usize;
    let mut arity = 1_usize;
    let mut segment_has_code = false;
    while cursor < bytes.len() {
        let tail = bytes.get(cursor..)?;
        let byte = *tail.first()?;
        if byte == b'%' {
            cursor = tail
                .iter()
                .position(|candidate| *candidate == b'\n')
                .map_or(bytes.len(), |length| cursor + length);
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            let delimiter = byte;
            let mut closed = false;
            cursor += 1;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'\\' => cursor = cursor.checked_add(2)?,
                    current if current == delimiter => {
                        cursor += 1;
                        closed = true;
                        break;
                    }
                    _ => cursor += 1,
                }
            }
            if !closed {
                return None;
            }
            segment_has_code = true;
            continue;
        }
        if byte == b'$' {
            cursor = cursor.checked_add(if tail.get(1) == Some(&b'\\') { 3 } else { 2 })?;
            if cursor > bytes.len() {
                return None;
            }
            segment_has_code = true;
            continue;
        }
        if tail.starts_with(b"<<") {
            binaries = binaries.checked_add(1)?;
            cursor += 2;
            segment_has_code = true;
            continue;
        }
        if tail.starts_with(b">>") && binaries > 0 {
            binaries -= 1;
            cursor += 2;
            continue;
        }
        match byte {
            b'(' => parens = parens.checked_add(1)?,
            b')' => parens = parens.checked_sub(1)?,
            b'[' => brackets = brackets.checked_add(1)?,
            b']' => brackets = brackets.checked_sub(1)?,
            b'{' => braces = braces.checked_add(1)?,
            b'}' => braces = braces.checked_sub(1)?,
            b',' if parens == 0 && brackets == 0 && braces == 0 && binaries == 0 => {
                if !segment_has_code {
                    return None;
                }
                arity = arity.checked_add(1)?;
                segment_has_code = false;
            }
            _ if !byte.is_ascii_whitespace() => segment_has_code = true,
            _ => {}
        }
        cursor += 1;
    }
    if parens != 0 || brackets != 0 || braces != 0 || binaries != 0 {
        return None;
    }
    if !segment_has_code {
        return (arity == 1).then_some(0);
    }
    Some(arity)
}

#[cfg(test)]
mod tests {
    use super::{canonical_atom, canonical_function_head, function_head};

    #[test]
    fn arity_distinguishes_overloads_and_ignores_nested_commas() {
        for (source, expected) in [
            ("work()", "work/0"),
            ("work(X)", "work/1"),
            ("work(X, Y)", "work/2"),
            ("work(#{a => [x,y]}, <<1,2>>, call(a,b))", "work/3"),
            ("work(\"a,b\", $,, X)", "work/3"),
            ("'work'(X)", "work/1"),
            ("'a-b'(X, Y)", "'a-b'/2"),
            ("run@node(X)", "run@node/1"),
        ] {
            assert_eq!(
                canonical_function_head(source, 128).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn malformed_or_dynamic_heads_have_no_identity() {
        for source in [
            "Module:work(X)",
            "?NAME(X)",
            "work(X,)",
            "work(,X)",
            "work(<<1,2>)",
            "work('unterminated)",
            "Work(X)",
        ] {
            assert!(function_head(source).is_none(), "{source}");
        }
        assert_eq!(canonical_atom("'work'", 6).as_deref(), Some("work"));
        assert_eq!(canonical_atom("'work'", 5), None);
        assert_eq!(canonical_atom("work", 3), None);
    }
}
