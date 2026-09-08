//! Direct Dart library imports over admitted source snapshots.
//! Namespace filters preserve ordered combinators; URI matching never guesses
//! package roots or binds an unsupported directive to a similarly named file.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ImportNamespace {
    prefix: Option<String>,
    filters: Vec<Filter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Filter {
    Show(BTreeSet<String>),
    Hide(BTreeSet<String>),
}

impl ImportNamespace {
    pub(super) fn admits(&self, qualifier: Option<&str>, name: &str) -> bool {
        self.prefix.as_deref() == qualifier
            && self.prefix.as_deref() != Some("_")
            && !name.starts_with('_')
            && self.filters.iter().all(|filter| match filter {
                Filter::Show(names) => names.contains(name),
                Filter::Hide(names) => !names.contains(name),
            })
    }
}

pub(super) fn parse_import(text: &str) -> Option<(String, ImportNamespace)> {
    let mut remaining = skip_trivia(text)?;
    if word(&mut remaining)? != "import" {
        return None;
    }
    remaining = skip_trivia(remaining)?;
    let raw = remaining.starts_with('r');
    if raw {
        remaining = remaining.strip_prefix('r')?;
    }
    let quote = remaining.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    remaining = remaining.get(quote.len_utf8()..)?;
    let end = remaining.find(quote)?;
    let uri = remaining.get(..end)?;
    // Escaped/interpolated URIs need Dart string evaluation, not path guessing.
    if uri.is_empty() || (!raw && uri.contains(['\\', '$'])) || uri.contains(['\r', '\n']) {
        return None;
    }
    remaining = remaining.get(end + quote.len_utf8()..)?;
    let mut namespace = ImportNamespace {
        prefix: None,
        filters: Vec::new(),
    };
    remaining = skip_trivia(remaining)?;
    if take_keyword(&mut remaining, "deferred")? {
        if !take_keyword(&mut remaining, "as")? {
            return None;
        }
        namespace.prefix = Some(word(&mut remaining)?.to_owned());
    } else if take_keyword(&mut remaining, "as")? {
        namespace.prefix = Some(word(&mut remaining)?.to_owned());
    }
    loop {
        remaining = skip_trivia(remaining)?;
        if let Some(tail) = remaining.strip_prefix(';') {
            return skip_trivia(tail)?
                .is_empty()
                .then(|| (uri.to_owned(), namespace));
        }
        let kind = word(&mut remaining)?;
        if !matches!(kind, "show" | "hide") {
            return None;
        }
        let mut names = BTreeSet::new();
        loop {
            names.insert(word(&mut remaining)?.to_owned());
            remaining = skip_trivia(remaining)?;
            let Some(tail) = remaining.strip_prefix(',') else {
                break;
            };
            remaining = tail;
        }
        namespace.filters.push(if kind == "show" {
            Filter::Show(names)
        } else {
            Filter::Hide(names)
        });
    }
}

pub(super) fn relative_uri(current_path: &str, uri: &str) -> Option<String> {
    if uri.is_empty() || uri.starts_with('/') || uri.contains([':', '\\', '?', '#', '%']) {
        return None;
    }
    let mut components = current_path
        .rsplit_once('/')
        .map_or_else(Vec::new, |(parent, _)| parent.split('/').collect());
    for component in uri.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            value => components.push(value),
        }
    }
    Some(components.join("/"))
}

pub(super) fn call_name(text: &str) -> Option<(&str, Option<&str>)> {
    let mut remaining = text;
    let first = word(&mut remaining)?;
    remaining = skip_trivia(remaining)?;
    if remaining.is_empty() {
        return Some((first, None));
    }
    remaining = remaining.strip_prefix('.')?;
    let name = word(&mut remaining)?;
    skip_trivia(remaining)?
        .is_empty()
        .then_some((name, Some(first)))
}

fn word<'a>(remaining: &mut &'a str) -> Option<&'a str> {
    *remaining = skip_trivia(remaining)?;
    let first = remaining.chars().next()?;
    if !(first.is_alphabetic() || matches!(first, '_' | '$')) {
        return None;
    }
    let end = remaining
        .char_indices()
        .find_map(|(offset, c)| {
            (!(c.is_alphanumeric() || matches!(c, '_' | '$'))).then_some(offset)
        })
        .unwrap_or(remaining.len());
    let value = remaining.get(..end)?;
    *remaining = remaining.get(end..)?;
    Some(value)
}

fn take_keyword(remaining: &mut &str, expected: &str) -> Option<bool> {
    let mut tail = skip_trivia(remaining)?;
    if word(&mut tail) == Some(expected) {
        *remaining = tail;
        Some(true)
    } else {
        Some(false)
    }
}

fn skip_trivia(mut text: &str) -> Option<&str> {
    loop {
        text = text.trim_start();
        if let Some(comment) = text.strip_prefix("//") {
            text = comment.get(comment.find(['\r', '\n']).unwrap_or(comment.len())..)?;
        } else if let Some(mut comment) = text.strip_prefix("/*") {
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
    fn imports_apply_every_combinator_and_library_privacy() {
        let (uri, namespace) = parse_import("import /* outer /* inner */ end */ 'api.dart' as api show run, skip, _secret hide skip show run, _secret;").unwrap();
        assert_eq!(uri, "api.dart");
        assert!(namespace.admits(Some("api"), "run"));
        for name in ["skip", "_secret", "other"] {
            assert!(!namespace.admits(Some("api"), name));
        }
        assert!(!namespace.admits(None, "run"));
        assert!(!namespace.admits(Some("elsewhere"), "run"));
        assert!(
            !parse_import("import 'api.dart' as _;")
                .unwrap()
                .1
                .admits(Some("_"), "run")
        );
        assert!(
            parse_import("import r'api.dart' deferred as api;")
                .unwrap()
                .1
                .admits(Some("api"), "run")
        );
    }

    #[test]
    fn unsupported_or_malformed_directives_do_not_become_bindings() {
        for source in [
            "export 'api.dart';",
            "part 'api.dart';",
            "import 'api.dart' if (dart.library.io) 'io.dart';",
            "import '$module.dart';",
            "import 'a\\x2edart';",
            "import 'a.dart' as;",
            "import 'a.dart' show;",
            "import 'a.dart' hide x,;",
            "import 'a.dart'; junk",
            "import /* unfinished",
        ] {
            assert!(parse_import(source).is_none(), "{source}");
        }
    }

    #[test]
    fn relative_library_uris_are_exact_and_confined() {
        for (uri, expected) in [
            ("api.dart", "lib/src/api.dart"),
            ("./api.dart", "lib/src/api.dart"),
            ("../api.dart", "lib/api.dart"),
            ("../../api.dart", "api.dart"),
        ] {
            assert_eq!(
                relative_uri("lib/src/main.dart", uri).as_deref(),
                Some(expected)
            );
        }
        for uri in [
            "../../../api.dart",
            "package:p/api.dart",
            "dart:core",
            "/api.dart",
            "C:/api.dart",
            "..\\api.dart",
            "api.dart?q",
            "api.dart#f",
            "%2e%2e/api.dart",
        ] {
            assert!(relative_uri("lib/src/main.dart", uri).is_none(), "{uri}");
        }
    }
}
