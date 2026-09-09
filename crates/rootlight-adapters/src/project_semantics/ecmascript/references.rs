//! Qualified reference paths reconstructed from native member-expression ranges.
//! The terminal occurrence retains its authored leaf span; missing path evidence
//! cannot fall back to an unrelated lexical declaration with the same name.

use super::*;

pub(in crate::project_semantics) fn is_member_path(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "javascript.member_path.reference" | "typescript.member_path.reference"
        )
}

pub(in crate::project_semantics) fn is_member_name(kind: &str) -> bool {
    matches!(
        kind,
        "javascript.member_name.reference"
            | "typescript.member_name.reference"
            | "typescript.type_member_name.reference"
            | "typescript.type_namespace_member.reference"
            | "typescript.type_query_member_name.reference"
    )
}

pub(in crate::project_semantics) fn is_type_position(occurrence: &OccurrenceDraft) -> bool {
    occurrence.role == OccurrenceRole::TypeUse
        || matches!(
            occurrence.syntax_kind.as_str(),
            "typescript.type_query_value.reference"
                | "typescript.type_query_member_name.reference"
                | "typescript.type_namespace_root.reference"
                | "typescript.type_namespace_member.reference"
        )
}

pub(in crate::project_semantics) fn member_paths(
    facts: &[SyntaxFact],
    cancellation: &Cancellation,
) -> Result<BTreeMap<u64, Option<SourceSpan>>, AdapterError> {
    cancellation.check()?;
    let mut paths = BTreeMap::new();
    for fact in facts.iter().filter(|fact| is_member_path(fact)) {
        cancellation.check()?;
        paths
            .entry(fact.span().end_byte())
            .and_modify(|known| {
                if *known != Some(fact.span()) {
                    *known = None;
                }
            })
            .or_insert(Some(fact.span()));
    }
    Ok(paths)
}

/// Normalizes only identifier-member paths admitted by native expression fields.
///
/// # Errors
/// Returns cancellation without producing a partial path.
pub(in crate::project_semantics) fn parse_member_path(
    text: &str,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<Option<(String, String)>, AdapterError> {
    cancellation.check()?;
    if text.len() > maximum {
        return Ok(None);
    }
    let mut tail = text;
    let mut parts = Vec::new();
    loop {
        cancellation.check()?;
        let Some(rest) = skip_trivia(tail, cancellation)? else {
            return Ok(None);
        };
        let end = rest
            .find(|ch: char| ch == '.' || ch == '?' || ch == '/' || is_trivia_space(ch))
            .unwrap_or(rest.len());
        let Some(name) = rest.get(..end).filter(|name| is_identifier(name)) else {
            return Ok(None);
        };
        parts.push(name);
        let Some(rest) = rest
            .get(end..)
            .map(|rest| skip_trivia(rest, cancellation))
            .transpose()?
            .flatten()
        else {
            return Ok(None);
        };
        if rest.is_empty() {
            break;
        }
        let Some(rest) = rest.strip_prefix("?.").or_else(|| rest.strip_prefix('.')) else {
            return Ok(None);
        };
        tail = rest;
    }
    let Some(name) = parts.pop().filter(|_| !parts.is_empty()) else {
        return Ok(None);
    };
    Ok(Some((parts.join("."), name.to_owned())))
}

fn skip_trivia<'a>(
    mut text: &'a str,
    cancellation: &Cancellation,
) -> Result<Option<&'a str>, AdapterError> {
    loop {
        cancellation.check()?;
        text = text.trim_start_matches(is_trivia_space);
        if let Some(rest) = text.strip_prefix("//") {
            text = rest
                .find(['\r', '\n', '\u{2028}', '\u{2029}'])
                .and_then(|end| rest.get(end..))
                .unwrap_or("");
        } else if let Some(rest) = text.strip_prefix("/*") {
            let Some(end) = rest.find("*/").and_then(|end| end.checked_add(2)) else {
                return Ok(None);
            };
            let Some(rest) = rest.get(end..) else {
                return Ok(None);
            };
            text = rest;
        } else {
            return Ok(Some(text));
        }
    }
}

fn is_trivia_space(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\u{000b}' | '\u{000c}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
                | '\r'
                | '\n'
                | '\u{2028}'
                | '\u{2029}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_member_paths_normalize_trivia_without_interpreting_expressions() {
        for (source, expected) in [
            ("Space.Public", Some(("Space", "Public"))),
            (
                "Space\u{feff} /* . skipped */ ?. Public",
                Some(("Space", "Public")),
            ),
            (
                "Space // ignored.name\r\n . Nested . Public",
                Some(("Space.Nested", "Public")),
            ),
            ("名.値", Some(("名", "値"))),
            ("Space().Public", None),
            ("Space['Public']", None),
            ("Space..Public", None),
            ("Space. /* incomplete", None),
            ("Space\u{0085}.Public", None),
            ("Space.123", None),
            ("'Space.Public'", None),
            ("Space.", None),
        ] {
            let actual = parse_member_path(source, 1024, &Cancellation::new()).unwrap();
            assert_eq!(
                actual
                    .as_ref()
                    .map(|(qualifier, name)| (qualifier.as_str(), name.as_str())),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn member_path_limits_and_cancellation_do_not_return_partial_bindings() {
        let source = "Space.Public";
        assert!(
            parse_member_path(source, source.len(), &Cancellation::new())
                .unwrap()
                .is_some()
        );
        assert!(
            parse_member_path(source, source.len() - 1, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            parse_member_path(source, 0, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
