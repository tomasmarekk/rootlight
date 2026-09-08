//! Source identities for grammar-reviewed SQL names and qualified object paths.
//! Trivia is removable, but case, quoting and Unicode remain dialect-dependent;
//! this boundary must not manufacture database name-resolution equivalence.

use std::borrow::Cow;

pub(crate) fn canonical_sql_name(text: &str, maximum_bytes: usize) -> Option<Cow<'_, str>> {
    if text.len() > maximum_bytes || text.contains('\0') {
        return None;
    }
    let mut remaining = skip_trivia(text)?;
    let first = remaining;
    let mut normalized = String::new();
    let mut changed = false;
    let mut end = 0_usize;
    loop {
        let (name, rest) = take_name(remaining)?;
        if changed {
            normalized.push_str(name);
        }
        end = end.checked_add(name.len())?;
        let next = skip_trivia(rest)?;
        if next.is_empty() {
            return if changed {
                Some(Cow::Owned(normalized))
            } else {
                Some(Cow::Borrowed(first.get(..end)?))
            };
        }
        let after_dot = next.strip_prefix('.')?;
        let next_name = skip_trivia(after_dot)?;
        if !changed && (rest != next || after_dot != next_name) {
            normalized.try_reserve_exact(text.len()).ok()?;
            normalized.push_str(first.get(..end)?);
            changed = true;
        }
        if changed {
            normalized.push('.');
        }
        end = end.checked_add(1)?;
        remaining = next_name;
    }
}

fn take_name(text: &str) -> Option<(&str, &str)> {
    let first = text.chars().next()?;
    let end = if matches!(first, '"' | '`' | '\'') {
        // Keep escape spelling intact: the grammar/dialect owns its meaning.
        let mut characters = text.char_indices().skip(1).peekable();
        loop {
            let (offset, character) = characters.next()?;
            if character == first {
                if characters.peek().is_some_and(|(_, next)| *next == first) {
                    characters.next();
                } else {
                    break offset.checked_add(character.len_utf8())?;
                }
            }
        }
    } else {
        let end = text
            .char_indices()
            .find_map(|(offset, character)| (!bare_character(character)).then_some(offset))
            .unwrap_or(text.len());
        if end == 0 || first.is_ascii_digit() {
            return None;
        }
        end
    };
    Some((text.get(..end)?, text.get(end..)?))
}

fn bare_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '@' | '$')
}

fn skip_trivia(mut text: &str) -> Option<&str> {
    loop {
        text = text.trim_start_matches(char::is_whitespace);
        if let Some(comment) = text.strip_prefix("--") {
            text = match comment.find('\n') {
                Some(end) => comment.get(end..)?,
                None => "",
            };
        } else if let Some(comment) = text.strip_prefix("/*") {
            // The pinned SQL grammar's marginalia token is non-nesting.
            text = comment.get(comment.find("*/")?.checked_add(2)?..)?;
        } else {
            return Some(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_names_preserve_quoting_case_and_component_boundaries() {
        for source in [
            "account",
            "Account",
            "@value",
            "store.account",
            "db.store.account",
            "\"Order Items\"",
            "\"store.account\"",
            "store.\"account\"",
            "`account`",
            "\"a\"\"b\"",
            "'a''b'",
            "\"\"",
            "\"π Ж\"",
            "\"a/*b*/.c--d\"",
            "\"a\nb\"",
        ] {
            let name = canonical_sql_name(source, source.len()).unwrap();
            assert!(matches!(name, Cow::Borrowed(_)));
            assert_eq!(name, source);
            assert_eq!(canonical_sql_name(source, source.len() - 1), None);
        }
    }

    #[test]
    fn only_inter_component_trivia_changes_source_identity() {
        for (source, expected) in [
            (
                " schema /* note */ . \"Order Items\" ",
                "schema.\"Order Items\"",
            ),
            ("db . schema. item", "db.schema.item"),
            ("db.schema /* note */ . item", "db.schema.item"),
            ("schema-- dot in comment . ignored\n.item", "schema.item"),
            ("/* prefix */schema.item-- suffix", "schema.item"),
            ("\"a . b\" . \"c/*d*/\"", "\"a . b\".\"c/*d*/\""),
        ] {
            let name = canonical_sql_name(source, source.len()).unwrap();
            assert_eq!(name, expected);
            assert_eq!(canonical_sql_name(&name, name.len()).unwrap(), name);
        }
    }

    #[test]
    fn invalid_or_unbounded_names_never_become_partial_identities() {
        for source in [
            "", " ", ".a", "a.", "a..b", "a b", "a/*", "a/*x*/b", "a. --end", "\"open", "a.\"open",
            "a()", "a+b", "a;SELECT", "1", "a\0b", "\"a\0b\"",
        ] {
            assert_eq!(canonical_sql_name(source, 256), None, "{source:?}");
        }
        assert_eq!(canonical_sql_name("a /*bounded source*/ . b", 3), None);
    }
}
