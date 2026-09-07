//! Exercises scalar value equivalence independently of document ownership.
//! Generic fixtures distinguish schema types and retain every source spelling.

use super::*;

fn key(source: &str) -> String {
    canonical_flow_key(source, 1024).unwrap_or_else(|| panic!("invalid fixture: {source:?}"))
}

#[test]
fn flow_string_spellings_share_value_without_losing_type_or_unicode() {
    for (source, expected) in [
        ("name", "str:\"name\""),
        ("'name'", "str:\"name\""),
        (r#""\x6eame""#, "str:\"name\""),
        (r#""\u006eame""#, "str:\"name\""),
        ("'here''s'", "str:\"here's\""),
        (r#"'\n'"#, r#"str:"\\n""#),
        ("''", "str:\"\""),
        ("' '", "str:\" \""),
        ("'true'", "str:\"true\""),
        ("'null'", "str:\"null\""),
        ("yes", "str:\"yes\""),
        ("on", "str:\"on\""),
        ("1_000", "str:\"1_000\""),
        ("-0x10", "str:\"-0x10\""),
        ("2000-01-01", "str:\"2000-01-01\""),
        ("https://example.org/a#b", "str:\"https://example.org/a#b\""),
        ("not a collection", "str:\"not a collection\""),
    ] {
        assert_eq!(key(source), expected, "{source:?}");
        assert_eq!(
            crate::structural_captured_name_for_language("yaml", source, 1024).as_deref(),
            Some(expected)
        );
    }
    for (left, right) in [
        ("true", "'true'"),
        ("null", "'null'"),
        ("~", "''"),
        ("1", "'1'"),
        ("1", "1.0"),
        ("é", "e\u{301}"),
        (r#""\n""#, "'\\n'"),
        ("int:1", "1"),
    ] {
        assert_ne!(key(left), key(right), "{left:?} versus {right:?}");
    }
}

#[test]
fn core_numeric_types_preserve_arbitrary_precision_and_equivalent_values() {
    for (sources, expected) in [
        (vec!["null", "Null", "NULL", "~"], "null:null"),
        (vec!["true", "True", "TRUE"], "bool:true"),
        (vec!["false", "False", "FALSE"], "bool:false"),
        (vec!["0", "+00", "-0", "0o0", "0x0"], "int:0"),
        (vec!["11", "+0011", "0o13", "0xB"], "int:11"),
        (vec!["-11", "-0011"], "int:-11"),
        (
            vec![
                "18446744073709551616",
                "0x10000000000000000",
                "0o2000000000000000000000",
            ],
            "int:18446744073709551616",
        ),
        (
            vec!["1.0", "10e-1", "+0.010e2", "100.000E-002"],
            "float:1e0",
        ),
        (vec!["-2.5", "-25E-1", "-0.025e+02"], "float:-25e-1"),
        (vec!["0.0", "-0.0", "0e999999999999999999999999"], "float:0"),
        (vec![".inf", "+.Inf", ".INF"], "float:inf"),
        (vec!["-.inf", "-.Inf", "-.INF"], "float:-inf"),
        (vec![".nan", ".NaN", ".NAN"], "float:nan"),
        (
            vec!["10e999999999999999999999", "1e1000000000000000000000"],
            "float:1e1000000000000000000000",
        ),
        (
            vec!["0.1e-999999999999999999999", "1e-1000000000000000000000"],
            "float:1e-1000000000000000000000",
        ),
    ] {
        for source in sources {
            assert_eq!(key(source), expected, "{source}");
        }
    }
    assert_ne!(key("9007199254740992.0"), key("9007199254740993.0"));
    for source in [
        "nUlL", "tRuE", ".Nan", "+.nan", "-0o7", "0Xff", "0o8", "0xG", "1e", "1e+", "1e2e3", ".",
        "1.2.3",
    ] {
        assert!(key(source).starts_with("str:"), "{source}");
    }
}

#[test]
fn radix_conversion_matches_independent_machine_integer_formatting() {
    for value in (0u128..=u128::from(u16::MAX)).chain([
        u128::from(u64::MAX),
        u128::from(u64::MAX) + 1,
        u128::MAX,
    ]) {
        let decimal = key(&value.to_string());
        assert_eq!(key(&format!("0x{value:x}")), decimal);
        assert_eq!(key(&format!("0o{value:o}")), decimal);
    }
}

#[test]
fn flow_string_decoding_agrees_with_an_independent_yaml_scanner() {
    use yaml_rust2::scanner::{Scanner, TokenType};

    let mut checked = 0;
    for quote in ["'", "\"", ""] {
        for left in ["first", "first ", "first\t", "🌍", "first\\t", "first\\ "] {
            for separator in [
                " ",
                "\n  ",
                "\r\n  ",
                "\r  ",
                "\n\n  ",
                "\n \n\n  ",
                "\u{85}",
                "\u{2028}",
            ] {
                for right in ["next", "next  ", "next\t", "next\\n"] {
                    if quote.is_empty() && right.ends_with([' ', '\t']) {
                        continue;
                    }
                    let source = format!("{quote}{left}{separator}{right}{quote}");
                    let mut scanner = Scanner::new(source.chars());
                    let mut values = Vec::new();
                    while let Some(token) = scanner
                        .next_token()
                        .unwrap_or_else(|error| panic!("{source:?}: {error}"))
                    {
                        match token.1 {
                            TokenType::Scalar(_, value) => values.push(value),
                            TokenType::StreamStart(_) | TokenType::StreamEnd => (),
                            other => panic!("unexpected token {other:?}: {source:?}"),
                        }
                    }
                    assert_eq!(values.len(), 1, "{source:?}");
                    let canonical = key(&source);
                    let json = canonical.strip_prefix("str:").unwrap();
                    let decoded: String = serde_json::from_str(json).unwrap();
                    assert_eq!(decoded, values[0], "{source:?}");
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 480);
}

#[test]
fn flow_folding_preserves_escaped_whitespace_and_only_yaml_line_breaks() {
    for (source, value) in [
        ("first \t\n  next", "first next"),
        ("first\r\n  \r\n  next", "first\nnext"),
        (
            "' first \n\n  second \t\r\n third '",
            " first\nsecond third ",
        ),
        ("\"first \\\n  next\"", "first next"),
        ("\"first\\\n\n  next\"", "first\nnext"),
        ("\"first\\ \n next\"", "first  next"),
        ("\"a\\t\n b\"", "a\t b"),
        ("'a\u{85}b\u{2028}c\u{2029}d'", "a\u{85}b\u{2028}c\u{2029}d"),
        ("\"\n  a\n  \"", " a "),
    ] {
        let mut expected = String::from("str:\"");
        for character in value.chars() {
            append_character(&mut expected, character, 1024).unwrap();
        }
        expected.push('"');
        assert_eq!(key(source), expected, "{source:?}");
    }
}

#[test]
fn yaml_escape_table_and_all_bmp_scalars_preserve_exact_values() {
    for (escape, character) in [
        ('0', '\0'),
        ('a', '\u{7}'),
        ('b', '\u{8}'),
        ('t', '\t'),
        ('n', '\n'),
        ('v', '\u{b}'),
        ('f', '\u{c}'),
        ('r', '\r'),
        ('e', '\u{1b}'),
        (' ', ' '),
        ('"', '"'),
        ('/', '/'),
        ('\\', '\\'),
        ('N', '\u{85}'),
        ('_', '\u{a0}'),
        ('L', '\u{2028}'),
        ('P', '\u{2029}'),
    ] {
        assert_eq!(
            key(&format!("\"\\{escape}\"")),
            key(&format!("\"\\U{:08X}\"", u32::from(character)))
        );
    }
    for scalar in (0u32..=0xffff).chain([0x10000, 0x1f30d, 0x10ffff]) {
        let source = format!("\"\\U{scalar:08X}\"");
        let result = canonical_flow_key(&source, 32);
        let Some(character) = char::from_u32(scalar) else {
            assert!(result.is_none());
            continue;
        };
        let mut expected = String::from("str:\"");
        append_character(&mut expected, character, 32).unwrap();
        expected.push('"');
        assert_eq!(result.as_deref(), Some(expected.as_str()));
        if scalar <= 0xffff {
            assert_eq!(key(&format!("\"\\u{scalar:04x}\"")), expected);
        }
    }
}

#[test]
fn malformed_or_document_dependent_captures_are_not_guessed() {
    for source in [
        "",
        " ",
        "key ",
        " key",
        "key\n",
        "'key",
        "\"key",
        "'a'b'",
        "\"a\"b\"",
        "\"\\q\"",
        "\"\\u123z\"",
        "\"\\uD800\"",
        "\"\\uD83C\\uDF0D\"",
        "\"\\U00110000\"",
        "\"\\UFFFFFFFF\"",
        "!tag key",
        "!!str true",
        "&anchor key",
        "*anchor",
        "[one, two]",
        "{one: two}",
        "|\n  value",
        ">2-\n  value",
        "key: value",
        "key # comment",
        "#comment",
        ":",
        "-",
        "?",
        "%YAML 1.2",
    ] {
        assert!(canonical_flow_key(source, 128).is_none(), "{source:?}");
    }
    for control in (0u8..=31)
        .chain([127, 128, 159])
        .filter(|c| !matches!(c, 9 | 10 | 13))
    {
        for source in [
            format!("a{}b", char::from(control)),
            format!("'a{}b'", char::from(control)),
            format!("\"a{}b\"", char::from(control)),
        ] {
            assert!(canonical_flow_key(&source, 128).is_none(), "{source:?}");
        }
    }
}

#[test]
fn source_output_and_numeric_expansion_each_obey_the_same_budget() {
    for source in [
        "a",
        "true",
        "0xFFFFFFFF",
        "'\t'",
        "'🌍'",
        "1e999999999999999999999999999999",
        "' '",
    ] {
        let expected = key(source);
        let minimum = source.len().max(expected.len());
        assert_eq!(
            canonical_flow_key(source, minimum).as_deref(),
            Some(expected.as_str())
        );
        assert!(
            canonical_flow_key(source, minimum - 1).is_none(),
            "{source}"
        );
    }
    for digits in [8, 64, 256] {
        let source = format!("0x{}", "f".repeat(digits));
        let full = canonical_flow_key(&source, 1024).unwrap();
        assert!(full.len() > source.len());
        assert!(canonical_flow_key(&source, full.len() - 1).is_none());
        assert_eq!(canonical_flow_key(&source, full.len()).unwrap(), full);
    }
}

#[test]
fn display_remains_readable_without_replacing_typed_identity() {
    for (source, expected) in [
        ("name", "name"),
        ("'a b'", "a b"),
        ("''", "str:\"\""),
        ("' '", "str:\" \""),
        ("true", "bool:true"),
        ("12", "int:12"),
        (r#""\0""#, r#"str:"\u0000""#),
        (r#"'a"b'"#, "a\"b"),
    ] {
        let canonical = key(source);
        assert_eq!(
            crate::structural_display_name_for_language("yaml", &canonical),
            expected
        );
        assert_eq!(
            crate::structural_display_name_for_language("rust", &canonical),
            canonical
        );
        assert!(expected.len() <= canonical.len());
    }
}
