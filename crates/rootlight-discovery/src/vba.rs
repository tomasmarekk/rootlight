//! Source evidence for shared classic BASIC exports and the narrower VBA dialect.
//! Exported module headers establish only the family; a VBA7 external declaration
//! supplies dialect evidence. Comments and string contents establish neither.

pub(super) fn has_content_hint(source: &[u8]) -> bool {
    has_statement(source, external_declaration)
}

pub(super) fn has_classic_content_hint(source: &[u8]) -> bool {
    has_statement(source, exported_module_header)
}

fn has_statement(source: &[u8], matches: fn(&[u8]) -> bool) -> bool {
    let mut source = source.strip_prefix(b"\xef\xbb\xbf").unwrap_or(source);
    while !source.is_empty() {
        let start = source;
        loop {
            let end = source
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
                .unwrap_or(source.len());
            let (line, tail) = source.split_at(end);
            source = tail
                .strip_prefix(b"\r\n")
                .or_else(|| tail.strip_prefix(b"\r"))
                .or_else(|| tail.strip_prefix(b"\n"))
                .unwrap_or(tail);
            if source.is_empty() || !continued_line(line) {
                break;
            }
        }
        if matches(&start[..start.len() - source.len()]) {
            return true;
        }
    }
    false
}

fn exported_module_header(mut source: &[u8]) -> bool {
    if !next_word(&mut source).is_some_and(|word| word.eq_ignore_ascii_case(b"attribute")) {
        return false;
    }
    source = source.trim_ascii_start();
    let Some(name) = source.get(..7) else {
        return false;
    };
    if !name.eq_ignore_ascii_case(b"vb_name") {
        return false;
    }
    let Some(literal) = source[7..].trim_ascii_start().strip_prefix(b"=") else {
        return false;
    };
    let Some(literal) = literal.trim_ascii_start().strip_prefix(b"\"") else {
        return false;
    };
    let Some(end) = literal.iter().position(|byte| *byte == b'"') else {
        return false;
    };
    let Ok(identifier) = std::str::from_utf8(&literal[..end]) else {
        return false;
    };
    let mut characters = identifier.chars();
    if !characters.next().is_some_and(char::is_alphabetic)
        || !characters.all(|character| character.is_alphanumeric() || character == '_')
    {
        return false;
    }
    let tail = literal[end + 1..].trim_ascii();
    tail.is_empty() || tail.starts_with(b"'")
}

fn continued_line(line: &[u8]) -> bool {
    if !line.ends_with(b"_")
        || !line
            .get(line.len().saturating_sub(2))
            .is_some_and(u8::is_ascii_whitespace)
    {
        return false;
    }
    let trimmed = line.trim_ascii_start();
    if trimmed
        .get(..3)
        .is_some_and(|word| word.eq_ignore_ascii_case(b"rem"))
        && trimmed.get(3).is_some_and(u8::is_ascii_whitespace)
    {
        return false;
    }
    let mut quoted = false;
    for byte in line {
        if *byte == b'\'' && !quoted {
            return false;
        }
        if *byte == b'"' {
            quoted = !quoted;
        }
    }
    !quoted
}

fn next_word<'a>(source: &mut &'a [u8]) -> Option<&'a [u8]> {
    loop {
        *source = source.trim_ascii_start();
        let end = source
            .iter()
            .position(u8::is_ascii_whitespace)
            .unwrap_or(source.len());
        let (word, tail) = source.split_at(end);
        *source = tail;
        if word != b"_" || !matches!(tail.first(), Some(b'\r' | b'\n')) {
            return (!word.is_empty()).then_some(word);
        }
    }
}

fn external_declaration(mut source: &[u8]) -> bool {
    let Some(mut first) = next_word(&mut source) else {
        return false;
    };
    if first.eq_ignore_ascii_case(b"public") || first.eq_ignore_ascii_case(b"private") {
        let Some(next) = next_word(&mut source) else {
            return false;
        };
        first = next;
    }
    if !(first.eq_ignore_ascii_case(b"declare")
        && next_word(&mut source).is_some_and(|word| word.eq_ignore_ascii_case(b"ptrsafe"))
        && next_word(&mut source).is_some_and(|word| {
            word.eq_ignore_ascii_case(b"function") || word.eq_ignore_ascii_case(b"sub")
        })
        && next_word(&mut source).is_some_and(|word| {
            word.first().is_some_and(u8::is_ascii_alphabetic)
                && word
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        })
        && next_word(&mut source).is_some_and(|word| word.eq_ignore_ascii_case(b"lib")))
    {
        return false;
    }
    // All intervening physical lines were proven continuations. Keep the
    // library literal intact: whitespace and doubled quotes belong to it.
    loop {
        source = source.trim_ascii_start();
        let Some(tail) = source
            .strip_prefix(b"_\r\n")
            .or_else(|| source.strip_prefix(b"_\r"))
            .or_else(|| source.strip_prefix(b"_\n"))
        else {
            break;
        };
        source = tail;
    }
    let Some(mut literal) = source.strip_prefix(b"\"") else {
        return false;
    };
    while let Some((&byte, tail)) = literal.split_first() {
        if matches!(byte, b'\r' | b'\n') {
            return false;
        }
        if byte == b'"' {
            if let Some(tail) = tail.strip_prefix(b"\"") {
                literal = tail;
                continue;
            }
            return true;
        }
        literal = tail;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{has_classic_content_hint, has_content_hint};

    #[test]
    fn exported_headers_establish_only_the_classic_family() {
        for source in [
            "Attribute VB_Name = \"Meter\"\n",
            "\u{feff}  attribute\tVB_NAME=\"Métrique\"\r\n",
            "VERSION 1.0 CLASS\rAttribute VB_Name = \"Holder\" ' exported\r",
            "' ignored _\nAttribute VB_Name = \"Meter\"",
        ] {
            assert!(has_classic_content_hint(source.as_bytes()), "{source}");
            assert!(!has_content_hint(source.as_bytes()), "{source}");
        }
    }

    #[test]
    fn incomplete_and_noncode_headers_do_not_establish_the_family() {
        for source in [
            "' Attribute VB_Name = \"Meter\"",
            "Rem Attribute VB_Name = \"Meter\"",
            "text = \"Attribute VB_Name = \"\"Meter\"\"\"",
            "text = _\nAttribute VB_Name = \"Meter\"",
            "Attribute VB_NameSuffix = \"Meter\"",
            "Attribute VB_Name = \"Meter",
            "Attribute VB_Name = \"Meter\n\"",
            "Attribute VB_Name = \"\"",
            "Attribute VB_Name = \"1Meter\"",
            "Attribute VB_Name = \"Meter\" & value",
            "Attribute VB_Name = \"Meter\"\"suffix\"",
            "Attribute VB_Name = unquoted",
        ] {
            assert!(!has_classic_content_hint(source.as_bytes()), "{source}");
        }
    }

    #[test]
    fn external_declarations_supply_case_insensitive_dialect_evidence() {
        for source in [
            "Declare PtrSafe Function ReadClock Lib \"clock\" () As LongPtr\n",
            "\u{feff}  private\tdeclare ptrsafe sub Pause Lib \"clock\" ()\r\n",
            "Option Explicit\nPublic Declare PtrSafe Sub Pause Lib \"clock\" ()\n",
        ] {
            assert!(has_content_hint(source.as_bytes()), "{source}");
        }
    }

    #[test]
    fn common_syntax_and_noncode_do_not_establish_a_dialect() {
        for source in [
            "' Declare PtrSafe Function ReadClock Lib \"clock\" ()\n",
            "Rem Declare PtrSafe Function ReadClock Lib \"clock\" ()\n",
            "value = \"Declare PtrSafe Function ReadClock Lib \"\"clock\"\" ()\"\n",
            "[Declare PtrSafe Function ReadClock Lib \"clock\"] = 1\n",
            "Private Declare Function ReadClock Lib \"clock\" () As Long\n",
            "Dim value As LongPtr\n",
            "DeclarePtrSafe Function ReadClock Lib \"clock\" ()\n",
            "Declare PtrSafe FunctionX ReadClock Lib \"clock\" ()\n",
            "Declare PtrSafe Function ReadClock Lib\n",
            "Declare PtrSafe Function ReadClock Lib not_a_string\n",
        ] {
            assert!(!has_content_hint(source.as_bytes()), "{source}");
        }
    }

    #[test]
    fn continued_external_declarations_supply_dialect_evidence() {
        for newline in ["\n", "\r\n", "\r"] {
            for source in [
                "Private _\nDeclare PtrSafe Function ReadClock Lib \"clock\" ()",
                "Declare _\nPtrSafe _\nFunction _\nReadClock _\nLib _\n\"clock library\" ()",
                "Declare PtrSafe Sub Pause _\nLib \"clock\" ()",
                "' ignored _\nDeclare PtrSafe Sub Pause Lib \"clock\" ()",
                "Rem ignored _\nDeclare PtrSafe Sub Pause Lib \"clock\" ()",
                "Declare PtrSafe Sub Pause Lib \"clock\"\"library\" ()",
            ] {
                let source = source.replace('\n', newline);
                assert!(has_content_hint(source.as_bytes()), "{source}");
            }
        }
    }

    #[test]
    fn declaration_hints_respect_logical_lines_and_complete_library_literals() {
        for source in [
            "value = _\nDeclare PtrSafe Function ReadClock Lib \"clock\" ()",
            "Declare\nPtrSafe Function ReadClock Lib \"clock\" ()",
            "Declare _ ' not a continuation\nPtrSafe Sub Pause Lib \"clock\" ()",
            "Declare PtrSafe Function ReadClock Lib \"clock",
            "Declare PtrSafe Function ReadClock Lib \"clock\n\"",
            "Declare PtrSafe Function ReadClock Lib _",
            "Declare PtrSafe Function ReadClock Lib \"clock\"\"",
            "Declare _ PtrSafe Sub Pause Lib \"clock\" ()",
            "Declare PtrSafe _ Sub Pause Lib \"clock\" ()",
        ] {
            assert!(!has_content_hint(source.as_bytes()), "{source}");
        }
    }
}
