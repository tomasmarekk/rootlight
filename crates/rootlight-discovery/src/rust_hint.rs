//! Conservative Rust content evidence outside comments and quoted examples.
//! Shared `fn main` syntax alone does not establish a language; suffix and
//! configuration evidence remain independent of this bounded lexical hint.

pub(super) fn has_content_hint(source: &[u8]) -> bool {
    let Some(code) = masked_code(source) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&code) else {
        return false;
    };
    let distinctive = has_macro(&code)
        || text.lines().any(|line| {
            let line = line.trim();
            line.strip_prefix("use ")
                .is_some_and(|path| path.contains("::") && path.ends_with(';'))
        });
    text.lines().any(|line| {
        let line = line.trim_start();
        if let Some(declaration) = line.strip_prefix("pub struct ") {
            let name_len = declaration
                .bytes()
                .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                .count();
            let Some(first) = declaration.as_bytes().first() else {
                return false;
            };
            return (first.is_ascii_alphabetic() || *first == b'_')
                && declaration.get(name_len..).is_some_and(|tail| {
                    matches!(
                        tail.trim_start().as_bytes().first(),
                        Some(b'{' | b'(' | b';')
                    )
                });
        }
        let main = [
            "pub async fn main",
            "async fn main",
            "pub fn main",
            "fn main",
        ]
        .into_iter()
        .find_map(|prefix| line.strip_prefix(prefix));
        let Some(parameters) = main.and_then(|tail| tail.trim_start().strip_prefix('(')) else {
            return false;
        };
        let Some(body) = parameters.trim_start().strip_prefix(')') else {
            return false;
        };
        distinctive && body.trim_start().starts_with('{')
    })
}

fn has_macro(code: &[u8]) -> bool {
    code.iter().enumerate().any(|(index, byte)| {
        if *byte != b'!' {
            return false;
        }
        let before = code[..index].trim_ascii_end();
        let after = code[index + 1..].trim_ascii_start();
        before
            .last()
            .is_some_and(|last| last.is_ascii_alphanumeric() || *last == b'_')
            && matches!(after.first(), Some(b'(' | b'[' | b'{'))
    })
}

fn masked_code(source: &[u8]) -> Option<Vec<u8>> {
    let mut code = source.to_vec();
    let mut cursor = 0;
    while cursor < source.len() {
        let rest = &source[cursor..];
        let length = if rest.starts_with(b"//") {
            rest.iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or(rest.len())
        } else if rest.starts_with(b"/*") {
            block_comment_length(rest)?
        } else if let Some(length) = raw_string_length(rest) {
            length?
        } else if matches!(rest[0], b'\'' | b'"') {
            quoted_length(rest)?
        } else {
            cursor += 1;
            continue;
        };
        for byte in &mut code[cursor..cursor + length] {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = b' ';
            }
        }
        cursor += length;
    }
    Some(code)
}

fn block_comment_length(source: &[u8]) -> Option<usize> {
    let mut depth = 1usize;
    let mut cursor = 2;
    while cursor + 1 < source.len() {
        if source[cursor..].starts_with(b"/*") {
            depth += 1;
            cursor += 2;
        } else if source[cursor..].starts_with(b"*/") {
            depth -= 1;
            cursor += 2;
            if depth == 0 {
                return Some(cursor);
            }
        } else {
            cursor += 1;
        }
    }
    None
}

fn quoted_length(source: &[u8]) -> Option<usize> {
    let delimiter = source[0];
    let mut cursor = 1;
    while cursor < source.len() {
        match source[cursor] {
            b'\\' => cursor += 2,
            byte if byte == delimiter => return Some(cursor + 1),
            _ => cursor += 1,
        }
    }
    None
}

fn raw_string_length(source: &[u8]) -> Option<Option<usize>> {
    let prefix = [b"br".as_slice(), b"cr".as_slice(), b"r".as_slice()]
        .into_iter()
        .find(|prefix| source.starts_with(prefix))?;
    let tail = &source[prefix.len()..];
    let hashes = tail.iter().take_while(|byte| **byte == b'#').count();
    if tail.get(hashes) != Some(&b'"') {
        return None;
    }
    let opening = prefix.len() + hashes + 1;
    let closing = hashes + 1;
    Some(
        source[opening..]
            .windows(closing)
            .position(|window| window[0] == b'"' && window[1..].iter().all(|byte| *byte == b'#'))
            .map(|offset| opening + offset + closing),
    )
}

#[cfg(test)]
mod tests {
    use super::has_content_hint;

    #[test]
    fn distinctive_rust_items_remain_extensionless_content_evidence() {
        for source in [
            "fn main() { println!(\"written\"); }",
            "pub fn main() { core::assert_eq!(1, 1); }",
            "use std::io;\nfn main() {}",
            "pub struct Record { field: u8 }",
            "pub struct Unit;",
            "pub struct Tuple(u8);",
            "/* nested /* comment */ boundary */\nfn main() { todo!(); }",
        ] {
            assert!(has_content_hint(source.as_bytes()), "{source}");
        }
    }

    #[test]
    fn shared_headers_and_quoted_or_commented_examples_are_not_rust_evidence() {
        for source in [
            "pub fn main() { io.println(\"Hello from app!\") }",
            "export fn main() void = { fmt::println(\"hello\")!; };",
            "fn main() raises:\n    print(\"hello\")",
            "// pub struct Record { field: u8 }",
            "/* pub struct Record { field: u8 } */",
            "/* outer /* nested */ pub struct Record { field: u8 } */",
            "\"pub struct Record { field: u8 }\"",
            "r###\"pub struct Record { field: u8 }\"###",
            "br##\"pub struct Record { field: u8 }\"##",
            "cr#\"pub struct Record { field: u8 }\"#",
            "fn main() { let text = r#\"println!(hidden);\"#; }",
            "fn main() { /* println!(hidden); */ }",
            "fn main() raises:\n    text = \"println!(hidden);\"",
            "; fn main() { println!(hidden); }",
            "(defpackage :sample) ; fn main(\n",
            "/* unterminated\nfn main() { todo!(); }",
        ] {
            assert!(!has_content_hint(source.as_bytes()), "{source}");
        }
    }
}
