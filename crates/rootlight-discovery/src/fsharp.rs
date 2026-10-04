//! Bounded declaration evidence for the `.fs` suffix shared by F# and Forth.
//! A module or namespace followed by a declaration changes routing only;
//! literal text, comments, and mixed Forth definitions provide no such hint.

pub(super) fn has_content_hint(source: &[u8], truncated: bool) -> bool {
    if source.len() > super::MAX_CLASSIFICATION_BYTES {
        return false;
    }
    let source = match std::str::from_utf8(source) {
        Ok(source) => source,
        Err(error) if truncated && error.error_len().is_none() => {
            // Only a scalar split by the sample boundary may be ignored.
            let Ok(source) = std::str::from_utf8(&source[..error.valid_up_to()]) else {
                return false;
            };
            source
        }
        Err(_) => return false,
    };
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let Some(masked) = mask_quoted_and_comment_contents(source.as_bytes(), truncated) else {
        return false;
    };
    let Ok(source) = std::str::from_utf8(&masked) else {
        return false;
    };
    // The partial final line still supplies conflicting evidence, but cannot
    // supply a new declaration anchor whose end lies outside this sample.
    let evidence = if truncated {
        source.rfind('\n').map_or("", |end| &source[..end + 1])
    } else {
        source
    };
    let mut lines = evidence
        .lines()
        .filter(|line| !line.trim().is_empty())
        .peekable();
    if !lines
        .next()
        .is_some_and(|line| module_or_namespace(line.trim()))
    {
        return false;
    }
    let forth_definition = source.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next() == Some(":") && words.next().is_some()
    }) && (truncated || source.split_whitespace().any(|word| word == ";"));
    if forth_definition || has_forth_product_terminator(source) {
        return false;
    }
    while let Some(line) = lines.next() {
        if declaration(line, lines.peek().copied()) {
            return true;
        }
    }
    false
}

fn has_forth_product_terminator(source: &str) -> bool {
    let mut words = source.split_whitespace().peekable();
    let mut definition = false;
    let mut previous = None;
    while let Some(word) = words.next() {
        if word == ":" {
            definition = words.peek().is_some_and(|name| identifier(name));
        } else if word == ";" {
            // Unlike valid annotations and record fields, a type product
            // cannot end at '*' before the statement terminator.
            if definition && previous == Some("*") {
                return true;
            }
            definition = false;
        }
        previous = Some(word);
    }
    false
}

fn module_or_namespace(line: &str) -> bool {
    let Some((keyword, rest)) = line.split_once(char::is_whitespace) else {
        return false;
    };
    if !matches!(keyword, "module" | "namespace") {
        return false;
    }
    let rest = rest.trim();
    let rest = rest.strip_prefix("rec ").unwrap_or(rest);
    let name = if keyword == "module" {
        rest.strip_suffix('=').unwrap_or(rest).trim_end()
    } else {
        rest
    };
    name.split('.').all(identifier)
}

fn declaration(line: &str, next_line: Option<&str>) -> bool {
    let Some((keyword, rest)) = line.trim().split_once(char::is_whitespace) else {
        return false;
    };
    if !matches!(keyword, "let" | "type") {
        return false;
    }
    let Some((left, right)) = rest.split_once('=') else {
        return false;
    };
    let right = right.trim();
    if right.starts_with('=') {
        return false;
    }
    let mut words = left.split_whitespace();
    let mut name = words.next();
    while name.is_some_and(|word| {
        matches!(
            word,
            "rec" | "mutable" | "private" | "internal" | "public" | "inline"
        )
    }) {
        name = words.next();
    }
    if !name.is_some_and(identifier) {
        return false;
    }
    if !right.is_empty() {
        return true;
    }
    let Some(next_line) = next_line else {
        return false;
    };
    if next_line.trim_start().starts_with('=') {
        return false;
    }
    // Tabs have no declared offside width in this source-only recognizer.
    // Guessing one could turn an undecidable body into language evidence.
    match (space_indentation(line), space_indentation(next_line)) {
        (Some(head), Some(body)) => body > head,
        _ => false,
    }
}

fn space_indentation(line: &str) -> Option<usize> {
    let prefix = line.get(..line.len() - line.trim_start().len())?;
    prefix
        .bytes()
        .all(|byte| byte == b' ')
        .then_some(prefix.len())
}

fn identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_alphabetic() || character == '_')
        && characters
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '\''))
}

fn mask_quoted_and_comment_contents(source: &[u8], truncated: bool) -> Option<Vec<u8>> {
    let mut masked = source.to_vec();
    let mut index = 0_usize;
    while let Some(tail) = source.get(index..) {
        if tail.is_empty() {
            break;
        }
        let end = if tail.starts_with(b"``") {
            escaped_identifier_end(source, index)
        } else if tail.starts_with(b"//") {
            Some(
                index
                    + tail
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .unwrap_or(tail.len()),
            )
        } else if tail.starts_with(b"(*") {
            block_comment_end(source, index)
        } else if tail.starts_with(b"\"") {
            string_end(source, index)
        } else {
            index += 1;
            continue;
        };
        let complete = end.is_some();
        let end = match end {
            Some(end) => end,
            // Preserve earlier evidence without completing a sampled name or literal.
            None if truncated => source.len(),
            None => return None,
        };
        for byte in masked.get_mut(index..end)? {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = b' ';
            }
        }
        if complete && tail.starts_with(b"``") {
            // Keep the existence of a complete name without letting its
            // escaped contents create mixed-language evidence.
            *masked.get_mut(index)? = b'_';
        } else if complete && tail.starts_with(b"\"") {
            // A complete literal can be a binding RHS; its contents cannot
            // create a header, declaration, or conflicting Forth definition.
            *masked.get_mut(index)? = b'"';
        }
        index = end;
    }
    Some(masked)
}

fn escaped_identifier_end(source: &[u8], start: usize) -> Option<usize> {
    let inner = source.get(start + 2..)?;
    let end = inner.windows(2).position(|bytes| bytes == b"``")?;
    let content = inner.get(..end)?;
    (!content.is_empty() && !content.iter().any(|byte| matches!(*byte, b'\r' | b'\n')))
        .then_some(start + end + 4)
}

fn block_comment_end(source: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 2;
    let mut depth = 1_usize;
    while let Some(tail) = source.get(index..) {
        if tail.starts_with(b"(*") {
            depth += 1;
            index += 2;
        } else if tail.starts_with(b"*)") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return Some(index);
            }
        } else if tail.is_empty() {
            return None;
        } else {
            index += 1;
        }
    }
    None
}

fn string_end(source: &[u8], start: usize) -> Option<usize> {
    if source.get(start..)?.starts_with(b"\"\"\"") {
        let end = source
            .get(start + 3..)?
            .windows(3)
            .position(|bytes| bytes == b"\"\"\"")?;
        return Some(start + end + 6);
    }
    let verbatim = start.checked_sub(1).and_then(|index| source.get(index)) == Some(&b'@');
    let mut index = start + 1;
    while let Some(byte) = source.get(index) {
        if *byte == b'"' {
            if verbatim && source.get(index + 1) == Some(&b'"') {
                index += 2;
            } else {
                return Some(index + 1);
            }
        } else if !verbatim && *byte == b'\\' {
            index += 2;
        } else {
            index += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::has_content_hint;

    #[test]
    fn fsharp_content_accepts_complete_indented_multiline_declarations() {
        for declaration in [
            "let value =\n    1",
            "let add left right =\n    left + right",
            "let rec private visit value =\n    value",
            "type Entry =\n    { Count: int }",
            "type Choice =\n    | Empty\n    | Item of int",
            "module Inner =\n    let value =\n        1",
            "let value = (* a complete comment *)\n\n    // no declaration evidence\n    (* outer (* nested *) comment *)\n    1",
            "let text =\n    \"module Ghost\\nlet ghost = 1\"",
            "let ``quoted value`` =\n    1",
        ] {
            let source = format!("module Example\n{declaration}");
            for ending in ["", "\n"] {
                for newline in ["\n", "\r\n"] {
                    let source = format!("{source}{ending}").replace('\n', newline);
                    assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
                }
            }
        }
    }

    #[test]
    fn fsharp_multiline_evidence_does_not_complete_missing_or_conflicting_rhs() {
        for declaration in [
            "let value =",
            "let value =\n    // no expression",
            "let value =\n    (* only a comment *)",
            "let value =\n1",
            "    let value =\n    1",
            "    let value =\n  1",
            "\tlet value =\n\t\t1",
            "let value =\n\t1",
            "let value =\n \t1",
            "let value =\n    = 1",
            "let value =\n    == 1",
            "let value =\n    \"unfinished",
            "let value =\n    (* unfinished",
            "let ``unfinished =\n    1",
            "let 1value =\n    1",
            "let value =\n    : square dup * ;",
            "type Entry =\n    : square dup * ;",
            "let value =\n    (* no expression *)\nmodule Other",
        ] {
            let source = format!("module Example\n{declaration}\n");
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
        }
        assert!(!has_content_hint(b"let value =\n    1\n", false));
        assert!(!has_content_hint(
            b"module Example\nlet value =\n    1",
            true
        ));
        assert!(!has_content_hint(
            b"module Example\nlet value =\n    \"unfinished\n",
            true,
        ));
        assert!(!has_content_hint(
            b"module Example\nlet value =\n    (* unfinished\n",
            true,
        ));
        assert!(!has_content_hint(
            b"module Example\nlet value =\n    ``unfinished\n",
            true,
        ));
        let mut outside = vec![b' '; super::super::MAX_CLASSIFICATION_BYTES];
        outside.extend_from_slice(b"module Example\nlet value =\n    1\n");
        assert!(!has_content_hint(
            &outside[..super::super::MAX_CLASSIFICATION_BYTES],
            true,
        ));
    }

    #[test]
    fn fsharp_content_keeps_complete_evidence_before_a_bounded_tail() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for (tail, remainder) in [
            ("let next = 123", "4\n"),
            ("let next = \"unfinished", "\"\n"),
            ("(* unfinished", " *)\n"),
            ("let ``unfinished", "`` = 2\n"),
        ] {
            let mut source = b"module Example\nlet value = 1\n".to_vec();
            source.resize(bound - tail.len(), b' ');
            source.extend_from_slice(tail.as_bytes());
            source.extend_from_slice(remainder.as_bytes());
            assert!(
                has_content_hint(&source[..bound], source.len() > bound),
                "{tail:?}"
            );
        }
        let mut source = b"module Example\nlet value = 1\n".to_vec();
        source.resize(bound - 1, b' ');
        source.extend_from_slice("\u{03bb}\n".as_bytes());
        assert!(has_content_hint(&source[..bound], true));
        assert!(!has_content_hint(&source[..bound], false));
    }

    #[test]
    fn fsharp_bounded_tail_cannot_create_an_anchor_or_hide_visible_forth() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for tail in [
            "let value = 12",
            "let value = \"unfinished",
            "let ``unfinished",
        ] {
            let mut source = b"module Example\n".to_vec();
            source.resize(bound - tail.len(), b' ');
            source.extend_from_slice(tail.as_bytes());
            assert!(!has_content_hint(&source, true), "{tail:?}");
        }
        for tail in [b": square".as_slice(), b": square dup * ;", b"\xff\n"] {
            let mut source = b"module Example\nlet value = 1\n".to_vec();
            source.extend_from_slice(tail);
            source.resize(bound, b' ');
            assert!(!has_content_hint(&source, true), "{tail:?}");
        }
    }

    #[test]
    fn fsharp_content_requires_an_opening_header_and_real_declaration_anchor() {
        for source in [
            "module Example.Core\nlet value = 1\n",
            "namespace Example.Core\ntype Entry = { Count: int }\n",
            "module Example =\n    let add x y = x + y\n",
            "\u{feff}(* outer (* nested *) comment *)\r\nmodule Example\r\nlet value = @\"a \"\"quoted\"\" literal\"\r\n",
            "module Example\nlet text = \"\"\"module Ghost\nlet ghost = 1\n: square dup * ;\n\"\"\"\n",
            "module Example\nlet value : int = 1 ;\n",
            "module Example\ntype Entry = { Pair : int * int ; Name : string }\n",
            "module Example\nlet value = [ 1 ] : int list ;\n",
            "module Example\nlet value = (1 : int) ;\n",
            "module Example\nlet value = 1\ntype Contract =\n    abstract Pair : int * int\n",
            "module Example\nlet ``value : square dup * ;`` = 1\n",
            "module ``Example : square dup * ;``\nlet value = 1\n",
            "module Example\nlet text = \": square dup * ;\"\n",
            "module Example\nlet text = @\": square \"\"dup\"\" * ;\"\n",
            "module Example\nlet value = 1 (* : square dup * ; *)\n",
        ] {
            assert!(has_content_hint(source.as_bytes(), false), "{source:?}");
        }
    }

    #[test]
    fn fsharp_content_rejects_forth_literal_comments_and_mixed_or_incomplete_evidence() {
        for source in [
            "",
            "module Example\n",
            "let value = 1\n",
            ": square dup * ;\n",
            ": square dup * ;\n// module Example\n// let value = 1\n",
            "(* module Example\nlet value = 1 *)\n",
            "\"module Example\nlet value = 1\"\n",
            "s\" module Example\nlet value = 1\"\n",
            ".\" module Example\nlet value = 1\"\n",
            "@\"module Example\nlet value = 1\"\n",
            "\"\"\"module Example\nlet value = 1\n\"\"\"\n",
            "module Example\nlet value = 1\n: square\n  dup * ;\n",
            "module Example\nlet value = 1 : square dup * ;\n",
            "module Example\nlet value = 1\n0 [if] : square dup * ; [then]\n",
            "module Example\n// let value = 1\n",
            "module Example\nlet value =\n",
            "module Example\nlet value == 1\n",
            "module Example\nlet value = \"unfinished\nlet other = 1\n",
            "module Example\nlet value = 1\n(* unfinished",
            "module Example\nlet value = 1\n``unfinished : square dup * ;",
        ] {
            assert!(!has_content_hint(source.as_bytes(), false), "{source:?}");
        }
    }

    #[test]
    fn fsharp_content_never_uses_incomplete_utf8_or_lexical_boundary_fragments() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for tail in ["λ", "\"unfinished", "(* unfinished", "let value ="] {
            let mut source = b"module Example\n".to_vec();
            source.resize(bound - 1, b' ');
            source.extend_from_slice(tail.as_bytes());
            assert!(!has_content_hint(&source[..bound], false));
        }
        let mut source = b"module Example\nlet value = 1\n".to_vec();
        source.resize(bound - 1, b' ');
        source.extend_from_slice("λ".as_bytes());
        assert!(!has_content_hint(&source[..bound], false));
    }
}
