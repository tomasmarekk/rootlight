//! Canonical names from grammar-reviewed Dart source captures.
//! Qualified names discard only inter-component trivia; exact written spans
//! remain with the caller and do not imply receiver or constructor resolution.

use std::borrow::Cow;

use crate::structural_captured_name;

/// Removes qualified-name trivia within the source and output byte budget.
///
/// Compact captures borrow their input. Invalid component boundaries, unfinished
/// nested comments, excessive source bytes or allocation failure return `None`.
pub(crate) fn canonical_dart_name(text: &str, maximum_bytes: usize) -> Option<Cow<'_, str>> {
    if text.len() > maximum_bytes {
        return None;
    }
    let text = text.trim();
    if matches!(text, "/" | "~/" | "[]" | "[]=") {
        return Some(Cow::Borrowed(text));
    }
    if let Some(name) = structural_captured_name(text, maximum_bytes) {
        return Some(Cow::Borrowed(name));
    }
    let mut normalized = String::new();
    normalized.try_reserve_exact(text.len()).ok()?;
    let mut remaining = skip_trivia(text)?;
    loop {
        let end = remaining
            .char_indices()
            .find_map(|(offset, character)| {
                (character.is_whitespace() || matches!(character, '.' | '/')).then_some(offset)
            })
            .unwrap_or(remaining.len());
        let component = remaining.get(..end)?;
        normalized.push_str(structural_captured_name(component, maximum_bytes)?);
        remaining = skip_trivia(remaining.get(end..)?)?;
        if remaining.is_empty() {
            return Some(Cow::Owned(normalized));
        }
        remaining = skip_trivia(remaining.strip_prefix('.')?)?;
        normalized.push('.');
    }
}

fn skip_trivia(mut text: &str) -> Option<&str> {
    loop {
        text = text.trim_start();
        if let Some(comment) = text.strip_prefix("//") {
            text = comment.get(comment.find(['\r', '\n']).unwrap_or(comment.len())..)?;
        } else if let Some(mut comment) = text.strip_prefix("/*") {
            // Dart block comments nest. Iterative scanning bounds stack use by
            // a counter while the already-admitted source bounds total work.
            let mut depth = 1usize;
            while depth != 0 {
                if let Some(rest) = comment.strip_prefix("/*") {
                    depth = depth.checked_add(1)?;
                    comment = rest;
                } else if let Some(rest) = comment.strip_prefix("*/") {
                    depth = depth.checked_sub(1)?;
                    comment = rest;
                } else {
                    comment = comment.get(comment.chars().next()?.len_utf8()..)?;
                }
            }
            text = comment;
        } else {
            return Some(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualifiers_discard_only_trivia_and_respect_the_source_budget() {
        for source in [
            "Store . named",
            "Store /* outer /* nested */ text */ . named",
            "Store // comment\r\n . named",
            " Store /* 🦀 */ .\n named ",
        ] {
            assert_eq!(
                canonical_dart_name(source, source.len()).as_deref(),
                Some("Store.named")
            );
            assert!(canonical_dart_name(source, source.len() - 1).is_none());
        }
        for source in [
            "Store.named",
            "Store.new",
            "value",
            "/",
            "~/",
            "[]",
            "[]=",
            "+",
        ] {
            assert!(
                matches!(canonical_dart_name(source, source.len()), Some(Cow::Borrowed(name)) if name == source)
            );
        }
    }

    #[test]
    fn malformed_trivia_does_not_merge_components_or_invent_names() {
        for source in [
            "Store /* unfinished . named",
            "Store /* outer /* inner */ . named",
            "Store /* gap */ named",
            "Store . /* gap */",
            "Store . . named",
            "Store ( ) . named",
        ] {
            assert!(
                canonical_dart_name(source, source.len()).is_none(),
                "{source}"
            );
        }
        assert_eq!(
            canonical_dart_name("Store // . named", 64).as_deref(),
            Some("Store")
        );
    }
}
