//! Conservative VBA dialect evidence for shared BASIC source suffixes.
//! Only a line-leading VBA7 external declaration is currently discriminating;
//! common BASIC syntax, comment text and string contents are not dialect proof.

pub(super) fn has_content_hint(source: &[u8]) -> bool {
    let source = source.strip_prefix(b"\xef\xbb\xbf").unwrap_or(source);
    source
        .split(|byte| matches!(byte, b'\r' | b'\n'))
        .any(|line| {
            let mut words = line
                .split(u8::is_ascii_whitespace)
                .filter(|word| !word.is_empty());
            let Some(mut first) = words.next() else {
                return false;
            };
            if first.eq_ignore_ascii_case(b"public") || first.eq_ignore_ascii_case(b"private") {
                let Some(next) = words.next() else {
                    return false;
                };
                first = next;
            }
            first.eq_ignore_ascii_case(b"declare")
                && words
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case(b"ptrsafe"))
                && words.next().is_some_and(|word| {
                    word.eq_ignore_ascii_case(b"function") || word.eq_ignore_ascii_case(b"sub")
                })
                && words.next().is_some_and(|word| {
                    word.first().is_some_and(u8::is_ascii_alphabetic)
                        && word
                            .iter()
                            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                })
                && words
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case(b"lib"))
                && words.next().is_some_and(|word| word.starts_with(b"\""))
        })
}

#[cfg(test)]
mod tests {
    use super::has_content_hint;

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
}
