//! Reads source-written VBA procedure-pointer names independently of native AST splits.
//! Only identifier paths and logical-line trivia are admitted; argument boundaries
//! are retained so a partial name cannot become an exact procedure reference.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;

pub(super) struct Path<'a> {
    pub(super) end: usize,
    pub(super) components: usize,
    pub(super) first: &'a [u8],
    pub(super) last: &'a [u8],
}

pub(super) fn parse<'a>(
    source: &'a [u8],
    cancellation: &Cancellation,
) -> Result<Option<Path<'a>>, AdapterError> {
    cancellation.check()?;
    let mut cursor = 0;
    let Some(first) = name(source, &mut cursor, cancellation)? else {
        return Ok(None);
    };
    let mut result = Path {
        end: cursor,
        components: 1,
        first,
        last: first,
    };
    loop {
        let before_trivia = cursor;
        let continued = trivia(source, &mut cursor, cancellation)?;
        match source.get(cursor) {
            Some(b'.') => {
                // MS-VBAL 5.6.12 requires adjacency or an explicit continuation
                // before the member operator, not arbitrary separating spaces.
                if cursor != before_trivia && !continued {
                    return Ok(None);
                }
                cursor += 1;
                trivia(source, &mut cursor, cancellation)?;
                let Some(last) = name(source, &mut cursor, cancellation)? else {
                    return Ok(None);
                };
                result.end = cursor;
                // Each component consumes at least one byte of the input.
                result.components += 1;
                result.last = last;
            }
            None | Some(b',' | b')' | b'\r' | b'\n' | b':' | b'\'') => return Ok(Some(result)),
            _ => return Ok(None),
        }
    }
}

fn name<'a>(
    source: &'a [u8],
    cursor: &mut usize,
    cancellation: &Cancellation,
) -> Result<Option<&'a [u8]>, AdapterError> {
    let quoted = source.get(*cursor) == Some(&b'[');
    if quoted {
        *cursor += 1;
    }
    let start = *cursor;
    if !source
        .get(*cursor)
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
    {
        return Ok(None);
    }
    while let Some(byte) = source.get(*cursor) {
        if cursor.is_multiple_of(512) {
            cancellation.check()?;
        }
        if !byte.is_ascii_alphanumeric() && *byte != b'_' {
            break;
        }
        *cursor += 1;
    }
    let name = source.get(start..*cursor);
    if quoted {
        if source.get(*cursor) != Some(&b']') {
            return Ok(None);
        }
        *cursor += 1;
    }
    Ok(name)
}

fn trivia(
    source: &[u8],
    cursor: &mut usize,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut continued = false;
    loop {
        cancellation.check()?;
        let start = *cursor;
        while matches!(source.get(*cursor), Some(b' ' | b'\t')) {
            if cursor.is_multiple_of(512) {
                cancellation.check()?;
            }
            *cursor += 1;
        }
        // A continuation needs separating whitespace. Ordinary newlines end
        // the argument and must not consume a following With-member statement.
        if *cursor == start || source.get(*cursor) != Some(&b'_') {
            return Ok(continued);
        }
        let mut end = *cursor + 1;
        while matches!(source.get(end), Some(b' ' | b'\t')) {
            if end.is_multiple_of(512) {
                cancellation.check()?;
            }
            end += 1;
        }
        if source.get(end) == Some(&b'\r') {
            end += 1;
        }
        if source.get(end) != Some(&b'\n') {
            return Ok(continued);
        }
        *cursor = end + 1;
        continued = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_keep_boundaries_quoting_and_logical_line_continuations() {
        for (source, target, first, last, components) in [
            (
                "Module.Callback, other",
                "Module.Callback",
                "Module",
                "Callback",
                2,
            ),
            (
                "[Module].[Callback])",
                "[Module].[Callback]",
                "Module",
                "Callback",
                2,
            ),
            (
                "Module _\r\n.Callback",
                "Module _\r\n.Callback",
                "Module",
                "Callback",
                2,
            ),
            (
                "Module. _\nCallback",
                "Module. _\nCallback",
                "Module",
                "Callback",
                2,
            ),
            (
                "Project.Module.Callback' note",
                "Project.Module.Callback",
                "Project",
                "Callback",
                3,
            ),
            ("Callback\n.Member", "Callback", "Callback", "Callback", 1),
        ] {
            let path = parse(source.as_bytes(), &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(source.get(..path.end), Some(target));
            assert_eq!(path.first, first.as_bytes());
            assert_eq!(path.last, last.as_bytes());
            assert_eq!(path.components, components);
        }
    }

    #[test]
    fn invalid_names_never_return_a_valid_prefix() {
        for source in [
            "",
            ".Callback",
            "Module.",
            "Module..Callback",
            "Module .Callback",
            "Module!Callback",
            "Module.Callback$",
            "Module.Callback(1)",
            "[Module.Callback",
            "Module.Callback + 1",
            "Module.[Callback ]",
            "Module.\nCallback",
            "Module _\n.Callback(",
        ] {
            assert!(
                parse(source.as_bytes(), &Cancellation::new())
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn paths_observe_cancellation_and_arbitrary_byte_boundaries() {
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(parse(b"Module.Callback", &cancellation).is_err());
        for byte in 0..=u8::MAX {
            for input in [vec![byte], vec![b'A', b'.', byte], vec![b'[', byte, b']']] {
                if let Some(path) = parse(&input, &Cancellation::new()).unwrap() {
                    assert!(path.end <= input.len());
                    assert!(path.components <= path.end);
                    assert!(!path.first.is_empty() && !path.last.is_empty());
                }
            }
        }
    }
}
