//! Bounded declaration evidence for the `.fs` suffix shared by F# and Forth.
//! A module or namespace followed by a declaration changes routing only;
//! literal text, comments, and mixed Forth definitions provide no such hint.

pub(super) fn has_content_hint(source: &[u8]) -> bool {
    if source.len() == super::MAX_CLASSIFICATION_BYTES && !source.ends_with(b"\n") {
        return false;
    }
    let Ok(source) = std::str::from_utf8(source) else {
        return false;
    };
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let Some(masked) = mask_quoted_and_comment_contents(source.as_bytes()) else {
        return false;
    };
    let Ok(source) = std::str::from_utf8(&masked) else {
        return false;
    };
    let mut lines = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    if !lines.next().is_some_and(module_or_namespace) {
        return false;
    }
    let forth_definition = source.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next() == Some(":") && words.next().is_some()
    }) && source.split_whitespace().any(|word| word == ";");
    !forth_definition && !has_forth_product_terminator(source) && lines.any(declaration)
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

fn declaration(line: &str) -> bool {
    let Some((keyword, rest)) = line.split_once(char::is_whitespace) else {
        return false;
    };
    if !matches!(keyword, "let" | "type") {
        return false;
    }
    let Some((left, right)) = rest.split_once('=') else {
        return false;
    };
    let right = right.trim();
    if right.is_empty() || right.starts_with('=') {
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
    name.is_some_and(identifier)
}

fn identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_alphabetic() || character == '_')
        && characters
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '\''))
}

fn mask_quoted_and_comment_contents(source: &[u8]) -> Option<Vec<u8>> {
    let mut masked = source.to_vec();
    let mut index = 0_usize;
    while let Some(tail) = source.get(index..) {
        if tail.is_empty() {
            break;
        }
        let end = if tail.starts_with(b"``") {
            escaped_identifier_end(source, index)?
        } else if tail.starts_with(b"//") {
            index
                + tail
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .unwrap_or(tail.len())
        } else if tail.starts_with(b"(*") {
            block_comment_end(source, index)?
        } else if tail.starts_with(b"\"") {
            string_end(source, index)?
        } else {
            index += 1;
            continue;
        };
        for byte in masked.get_mut(index..end)? {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = b' ';
            }
        }
        if tail.starts_with(b"``") {
            // Keep the existence of a complete name without letting its
            // escaped contents create mixed-language evidence.
            *masked.get_mut(index)? = b'_';
        } else if tail.starts_with(b"\"") {
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
            assert!(has_content_hint(source.as_bytes()), "{source:?}");
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
            assert!(!has_content_hint(source.as_bytes()), "{source:?}");
        }
    }

    #[test]
    fn fsharp_content_never_uses_incomplete_utf8_or_lexical_boundary_fragments() {
        let bound = super::super::MAX_CLASSIFICATION_BYTES;
        for tail in ["λ", "\"unfinished", "(* unfinished", "let value ="] {
            let mut source = b"module Example\n".to_vec();
            source.resize(bound - 1, b' ');
            source.extend_from_slice(tail.as_bytes());
            assert!(!has_content_hint(&source[..bound]));
        }
        let mut source = b"module Example\nlet value = 1\n".to_vec();
        source.resize(bound - 1, b' ');
        source.extend_from_slice("λ".as_bytes());
        assert!(!has_content_hint(&source[..bound]));
    }
}
