//! Bounded syntax evidence for the `.pl` suffix shared by Perl and Prolog.
//! A recognizable opening sentence changes routing only; it does not claim
//! that the remaining source parses or that its predicates are resolved.

pub(super) fn has_content_hint(source: &[u8]) -> bool {
    let source = match std::str::from_utf8(source) {
        Ok(source) => source,
        Err(error) if error.error_len().is_none() => {
            let Ok(prefix) = std::str::from_utf8(&source[..error.valid_up_to()]) else {
                return false;
            };
            prefix
        }
        Err(_) => return false,
    };
    let Some(source) = first_statement(source) else {
        return false;
    };
    if let Some(rest) = source
        .strip_prefix(":-")
        .or_else(|| source.strip_prefix("?-"))
    {
        return rest.trim_start().chars().next().is_some_and(|character| {
            character.is_alphabetic() || matches!(character, '(' | '\'' | '[' | '{')
        });
    }
    let Some(name_end) = atom_name_end(source) else {
        return false;
    };
    let Some(rest) = source.get(name_end..) else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '\t']);
    let rest = if rest.starts_with('(') {
        let Some(rest) = after_compound_arguments(rest) else {
            return false;
        };
        rest
    } else {
        rest
    };
    let rest = rest.trim_start_matches([' ', '\t', '\r', '\n']);
    rest.starts_with(":-") || rest.starts_with(":+") || sentence_dot(rest)
}

fn atom_name_end(source: &str) -> Option<usize> {
    if source.starts_with('\'') {
        let closing = source.get(1..)?.find('\'')?;
        let inner = source.get(1..closing + 1)?;
        // Routing must not claim escaped atoms that the static parser cannot
        // interpret independently of the mutable character_escapes flag.
        return (!inner.is_empty()
            && !inner.contains('\\')
            && !inner.chars().any(char::is_control))
        .then_some(closing + 2);
    }
    let first = source.chars().next()?;
    if !first.is_lowercase() && !(first.is_alphabetic() && !first.is_uppercase()) {
        return None;
    }
    source
        .char_indices()
        .take_while(|(_, character)| character.is_alphanumeric() || *character == '_')
        .last()
        .map(|(index, character)| index + character.len_utf8())
}

fn first_statement(mut source: &str) -> Option<&str> {
    source = source.strip_prefix('\u{feff}').unwrap_or(source);
    loop {
        source = source.trim_start();
        if source.starts_with('%') || source.starts_with("#!") {
            source = after_line(source);
        } else if let Some(comment) = source.strip_prefix("/*") {
            source = comment.split_once("*/")?.1;
        } else {
            return (!source.is_empty()).then_some(source);
        }
    }
}

fn after_line(source: &str) -> &str {
    source
        .split_once('\n')
        .map_or("", |(_, remainder)| remainder)
}

fn after_compound_arguments(source: &str) -> Option<&str> {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut depth = 0_usize;
    let mut quote = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                index = index.saturating_add(2);
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
        } else if bytes[index..].starts_with(b"/*") {
            let end = bytes[index + 2..]
                .windows(2)
                .position(|pair| pair == b"*/")?;
            index += end + 4;
            continue;
        } else if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
        } else if byte == b'(' {
            depth += 1;
        } else if byte == b')' {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return source.get(index + 1..);
            }
        }
        index += 1;
    }
    None
}

fn sentence_dot(source: &str) -> bool {
    let Some(rest) = source.strip_prefix('.') else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '\t']);
    rest.is_empty() || rest.starts_with(['\r', '\n', '%'])
}

#[cfg(test)]
mod tests {
    use super::has_content_hint;

    #[test]
    fn recognizes_only_source_backed_opening_sentences() {
        for source in [
            "% source comment\n:- use_module(library(lists)).\n",
            "/* license */\nreach(A, B) :-\n  edge(A, B).\n",
            "path(a, b).\n",
            "'<urn:edge>'(A, B) :- edge(A, B).\n",
            "'plain'(A).\n",
            "result('close ) here', [a, b]) .\n",
            "λ(1).\n",
            "#!/usr/bin/env swipl\nquery(X) :+ answer(X).\n",
            "/* outer */ /* inner */ atom.\n",
        ] {
            assert!(has_content_hint(source.as_bytes()), "{source}");
        }
    }

    #[test]
    fn rejects_perl_and_unproven_source() {
        for source in [
            "# comment\nuse strict;\nsub reach { 1 }\n",
            "#!/usr/bin/perl\nprint 'reach(a).';\n",
            "print(1) . \"text\";\n",
            "reach(A, B);\n",
            "Reach(A).\n",
            "% reach(A).\n",
            "/* unclosed comment\nreach(A).\n",
            "reach('unclosed).\n",
            "'escaped\\'name'(A).\n",
            "'string' . \"other\";\n",
            "result(1) .suffix\n",
            ":-\n",
        ] {
            assert!(!has_content_hint(source.as_bytes()), "{source}");
        }
    }

    #[test]
    fn utf8_classification_bound_may_end_inside_later_identifier() {
        let mut source = b"edge(alpha, beta).\n".to_vec();
        source.resize(super::super::MAX_CLASSIFICATION_BYTES - 1, b' ');
        source.extend_from_slice("λ".as_bytes());
        assert!(has_content_hint(
            &source[..super::super::MAX_CLASSIFICATION_BYTES]
        ));
    }
}
