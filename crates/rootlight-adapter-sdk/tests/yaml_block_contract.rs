//! Source-complete YAML block values, independently checked against scalar tokens.
//! EOF and document-start boundaries follow the specification directly: the
//! external scanner changes those values, so it is not their oracle.

use rootlight_adapter_sdk::{YamlBlockScalar, YamlDocumentContext};
use yaml_rust2::scanner::{Scanner, TScalarStyle, TokenType};

fn parse(source: &str, parent: Option<usize>) -> YamlBlockScalar<'_> {
    let start = source.find(['|', '>']).unwrap();
    YamlBlockScalar::parse(source, start..start + 1, parent, 32768)
        .unwrap_or_else(|| panic!("invalid fixture: {source:?}"))
}

fn oracle(source: &str) -> String {
    let mut scanner = Scanner::new(source.chars());
    let mut values = Vec::new();
    while let Some(token) = scanner
        .next_token()
        .unwrap_or_else(|error| panic!("{source:?}: {error}"))
    {
        if let TokenType::Scalar(TScalarStyle::Literal | TScalarStyle::Folded, value) = token.1 {
            values.push(value);
        }
    }
    assert_eq!(values.len(), 1, "{source:?}");
    values.remove(0)
}

#[test]
fn native_capture_tail_is_recovered_without_consuming_the_next_node() {
    for newline in ["\n", "\r\n", "\r"] {
        for style in ["|", "|-", "|+", ">", ">-", ">+"] {
            for trailing in [1, 2, 3] {
                let source = format!(
                    "first: {style}{newline}  alpha{}next: done{newline}",
                    newline.repeat(trailing)
                );
                let start = source.find(style).unwrap();
                let captured_end = source.find("alpha").unwrap() + 5;
                let scalar =
                    YamlBlockScalar::parse(&source, start..captured_end, Some(0), 256).unwrap();
                let end = source.find("next:").unwrap();
                assert_eq!(scalar.lexical_range(), start..end);
                assert_eq!(scalar.source_text(), &source[start..end]);
                assert!(scalar.lexical_range().end > captured_end);
                assert_eq!(scalar.value(), oracle(&source), "{source:?}");
            }
        }
    }
}

#[test]
fn literal_and_folded_values_preserve_indentation_empty_lines_and_unicode() {
    for (body, literal, folded) in [
        ("  first\n  second\n", "first\nsecond\n", "first second\n"),
        (
            "\n  first\n\n  second\n",
            "\nfirst\n\nsecond\n",
            "\nfirst\nsecond\n",
        ),
        (
            "  first\n    indented\n  second\n",
            "first\n  indented\nsecond\n",
            "first\n  indented\nsecond\n",
        ),
        (
            "  first\n\n    indented\n\n  second\n",
            "first\n\n  indented\n\nsecond\n",
            "first\n\n  indented\n\nsecond\n",
        ),
        (
            "  first \t\n  second\n",
            "first \t\nsecond\n",
            "first \t second\n",
        ),
        (
            "  🌍\n  é\u{85}e\u{2028}x\u{2029}y\n",
            "🌍\né\u{85}e\u{2028}x\u{2029}y\n",
            "🌍 é\u{85}e\u{2028}x\u{2029}y\n",
        ),
        (
            "  # content\n  \\n\n",
            "# content\n\\n\n",
            "# content \\n\n",
        ),
        (
            "  first\n   \n  second\n",
            "first\n \nsecond\n",
            "first\n \nsecond\n",
        ),
        (
            "  \tfirst\n  second\n",
            "\tfirst\nsecond\n",
            "\tfirst\nsecond\n",
        ),
    ] {
        for (style, expected) in [("|", literal), (">", folded)] {
            let source = format!("key: {style}\n{body}next: done\n");
            let scalar = parse(&source, Some(0));
            assert_eq!(scalar.value(), expected, "{source:?}");
            assert_eq!(scalar.value(), oracle(&source), "{source:?}");
        }
    }
}

#[test]
fn explicit_indentation_uses_parent_not_header_column() {
    for parent in [0, 1, 2, 7] {
        for digit in 1..=9 {
            for order in [format!("{digit}-"), format!("-{digit}")] {
                for style in ["|", ">"] {
                    let padding = " ".repeat(parent);
                    let source = format!(
                        "{padding}long_key_name: {style}{order} # header\n{} value\n{padding}next: done\n",
                        " ".repeat(parent + digit)
                    );
                    let scalar = parse(&source, Some(parent));
                    assert_eq!(scalar.value(), " value");
                    assert_eq!(scalar.value(), oracle(&source));
                }
            }
        }
    }
    for source in ["|2-\n   value\n...\n", "--- >2-\n   value\n...\n"] {
        let scalar = parse(source, None);
        assert_eq!(scalar.value(), " value");
        assert_eq!(scalar.value(), oracle(source));
    }
}

#[test]
fn empty_block_chomping_and_eof_never_invent_content() {
    for (source, expected) in [
        ("|", ""),
        (">-", ""),
        ("|+", ""),
        ("|\n", ""),
        ("|\n\n", ""),
        ("|+\n", ""),
        ("|+\n\n", "\n"),
        (">+\n\n\n", "\n\n"),
        ("|-\n  value", "value"),
        ("|\n  value", "value"),
        ("|+\n  value", "value"),
        (">\n  first\n  second", "first second"),
        ("|2+\n    ", "  "),
        ("|2+\n  ", ""),
        ("|+\n  ", ""),
    ] {
        let scalar = parse(source, None);
        assert_eq!(scalar.value(), expected, "{source:?}");
        assert_eq!(scalar.lexical_range().end, source.len());
    }
    for (style, expected) in [("|", ""), ("|-", ""), ("|+", "\n\n")] {
        let source = format!("key: {style}\n\n  \nnext: done\n");
        assert_eq!(parse(&source, Some(0)).value(), expected);
        assert_eq!(parse(&source, Some(0)).value(), oracle(&source));
    }
}

#[test]
fn root_markers_and_dedented_comments_are_not_scalar_content() {
    for marker in ["---", "...", "--- # next", "...\t# end"] {
        let source = format!("|+\nfirst\nsecond\n\n{marker}\n");
        let scalar = parse(&source, None);
        assert_eq!(scalar.value(), "first\nsecond\n\n");
        assert_eq!(scalar.lexical_range().end, source.find(marker).unwrap());
        if marker.starts_with("...") {
            assert_eq!(scalar.value(), oracle(&source));
        }
    }
    let source = "key: |+\n  # content\n\n # trailing\n    # still trailing\nnext: done\n";
    let scalar = parse(source, Some(0));
    assert_eq!(scalar.value(), "# content\n\n");
    assert_eq!(
        scalar.lexical_range().end,
        source.find(" # trailing").unwrap()
    );
    assert_eq!(scalar.value(), oracle(source));
    let source = "|\n---not-a-marker\n...still-content\n...\n";
    let scalar = parse(source, None);
    assert_eq!(scalar.value(), "---not-a-marker\n...still-content\n");
    let source = "key: |\n  value\n\t# trailing comment\nnext: done\n";
    let scalar = parse(source, Some(0));
    assert_eq!(scalar.value(), "value\n");
    assert_eq!(scalar.lexical_range().end, source.find('\t').unwrap());
    assert_eq!(scalar.value(), oracle(source));
}

#[test]
fn malformed_headers_indentation_and_captures_fail_closed() {
    for header in [
        "|0",
        "|10",
        "|++",
        "|--",
        "|+-",
        "|2+3",
        ">x",
        "|#comment",
        "| +",
        "|2 text",
        "|\0",
    ] {
        let source = format!("key: {header}\n  value\nnext: done\n");
        assert!(
            YamlBlockScalar::parse(&source, 5..6, Some(0), 128).is_none(),
            "{source:?}"
        );
    }
    for source in [
        "key: |\n    \n  value\nnext: done\n",
        "key: |\n    value\n  less\nnext: done\n",
        "key: |3\n  value\nnext: done\n",
        "key: |\n\tvalue\nnext: done\n",
        "key: |2\n \tvalue\nnext: done\n",
        "key: |\n  bad\0value\nnext: done\n",
    ] {
        assert!(
            YamlBlockScalar::parse(source, 5..6, Some(0), 128).is_none(),
            "{source:?}"
        );
    }
    let source = "🌍: |\n  value\nnext: done\n";
    for capture in [
        0..0,
        1..6,
        std::ops::Range { start: 6, end: 5 },
        6..usize::MAX,
        6..source.len(),
    ] {
        assert!(
            YamlBlockScalar::parse(source, capture.clone(), Some(0), 128).is_none(),
            "{capture:?}"
        );
    }
    assert!(YamlBlockScalar::parse("|2\n  value\n", 0..1, Some(usize::MAX), 128).is_none());
}

#[test]
fn block_values_use_the_same_document_tag_context_as_flow_values() {
    let context = YamlDocumentContext::new(None, &[("!t!", "tag:yaml.org,2002:")], 256).unwrap();
    let integer = parse("key: |-\n  12\nnext: done\n", Some(0));
    assert_eq!(
        context.block_scalar(&integer, None).unwrap().name(),
        "str:\"12\""
    );
    assert_eq!(
        context
            .block_scalar(&integer, Some("!t!int"))
            .unwrap()
            .name(),
        "int:12"
    );
    let clipped = parse("key: |\n  12\nnext: done\n", Some(0));
    assert!(context.block_scalar(&clipped, Some("!!int")).is_none());
    let opaque = context.block_scalar(&integer, Some("!type")).unwrap();
    assert!(opaque.has_unrecognized_tag());
    assert_eq!(opaque.name(), "tag:\"!type\":\"12\"");
    let empty = parse("key: |\nnext: done\n", Some(0));
    assert_eq!(
        context.block_scalar(&empty, None).unwrap().name(),
        "str:\"\""
    );
    assert_eq!(
        context.block_scalar(&empty, Some("!!null")).unwrap().name(),
        "null:null"
    );
}

#[test]
fn lexical_input_and_expanded_identity_have_exact_independent_bounds() {
    for source in [
        "key: |+\n  value\n\nnext: done\n",
        "key: |\n  🌍\nnext: done\n",
        "key: >-\n  first\n  second",
    ] {
        let scalar = parse(source, Some(0));
        let start = scalar.lexical_range().start;
        let bytes = scalar.source_text().len();
        assert!(YamlBlockScalar::parse(source, start..start + 1, Some(0), bytes).is_some());
        assert!(YamlBlockScalar::parse(source, start..start + 1, Some(0), bytes - 1).is_none());
        let context = YamlDocumentContext::new(None, &[], 1024).unwrap();
        let identity = context.block_scalar(&scalar, None).unwrap();
        let maximum = bytes.max(identity.name().len());
        assert!(
            YamlDocumentContext::new(None, &[], maximum)
                .unwrap()
                .block_scalar(&scalar, None)
                .is_some()
        );
        assert!(
            YamlDocumentContext::new(None, &[], maximum - 1)
                .unwrap()
                .block_scalar(&scalar, None)
                .is_none()
        );
    }
    let source = format!("key: |-\n  value\nnext: {}", "x".repeat(1_000_000));
    let scalar = YamlBlockScalar::parse(&source, 5..6, Some(0), 13).unwrap();
    assert_eq!(scalar.value(), "value");
    let source = format!("key: |-\n  {}", "x".repeat(1_000_000));
    assert!(YamlBlockScalar::parse(&source, 5..6, Some(0), 32).is_none());
}

#[test]
fn block_folding_matches_independent_scanner_across_line_patterns() {
    let lines = ["", "  first", "   indented", "  \ttext", "  🌍", "  end  "];
    let mut checked = 0;
    for style in ["|2", "|2-", "|2+", ">2", ">2-", ">2+"] {
        for newline in ["\n", "\r\n", "\r"] {
            for first in lines {
                for second in lines {
                    for third in lines {
                        let source = format!(
                            "key: {style}{newline}{first}{newline}{second}{newline}{third}{newline}next: done{newline}"
                        );
                        let scalar = parse(&source, Some(0));
                        assert_eq!(scalar.value(), oracle(&source), "{source:?}");
                        checked += 1;
                    }
                }
            }
        }
    }
    assert_eq!(checked, 3888);
}

#[test]
fn empty_scalar_trailing_comment_does_not_become_badly_indented_content() {
    for chomp in ["", "-", "+"] {
        let source = format!("empty: |{chomp}\n    \n   # trailing comment\nnext: done\n");
        let block = parse(&source, Some(0));
        assert_eq!(block.value(), if chomp == "+" { "\n" } else { "" });
        assert_eq!(block.lexical_range().end, source.find("   #").unwrap());
    }
    let source = "empty: |+\n    \n    # actual content\nnext: done\n";
    let block = parse(source, Some(0));
    assert_eq!(block.value(), "\n# actual content\n");
    assert_eq!(block.value(), oracle(source));
    let source = "--- |\n# root content\n...\n";
    assert_eq!(parse(source, None).value(), "# root content\n");
}

#[test]
fn implicit_indentation_matches_independent_scanner_with_leading_empty_lines() {
    let lines = ["", "  normal", "   indented", "  \ttext", "  🌍", "  end  "];
    let mut checked = 0;
    for style in ["|", "|-", "|+", ">", ">-", ">+"] {
        for newline in ["\n", "\r\n", "\r"] {
            for leading in ["", "\n", " \n", "  \n\n"] {
                for middle in lines {
                    for last in lines {
                        let source = format!(
                            "key: {style}{newline}{}  seed{newline}{middle}{newline}{last}{newline}next: done{newline}",
                            leading.replace('\n', newline)
                        );
                        let block = parse(&source, Some(0));
                        assert_eq!(block.value(), oracle(&source), "{source:?}");
                        checked += 1;
                    }
                }
            }
        }
    }
    assert_eq!(checked, 2592);
}

#[test]
fn scalar_source_mutations_preserve_checked_spans_and_bounded_identity() {
    let mut admitted = 0;
    for original in [
        "key: |+\n  🌍\n\nnext: done\n",
        "key: >2- # header\r\n  first\r\n   indented\r\nnext: done\r\n",
        "key: |\nnext: done\n",
        "key: |-\n  last",
    ] {
        for (at, _) in original.char_indices().chain([(original.len(), '\0')]) {
            for insert in ["", " ", "\t", "\r", "\n", "\0", "#", "🌍", "---\n", "...\n"] {
                let source = format!("{}{insert}{}", &original[..at], &original[at..]);
                let Some(start) = source.find(['|', '>']) else {
                    continue;
                };
                for maximum in [0, 1, 8, 32, 128, usize::MAX] {
                    let Some(block) =
                        YamlBlockScalar::parse(&source, start..start + 1, Some(0), maximum)
                    else {
                        continue;
                    };
                    let range = block.lexical_range();
                    assert_eq!(range.start, start);
                    assert!(range.end > start);
                    assert_eq!(source.get(range), Some(block.source_text()));
                    assert!(block.source_text().len() <= maximum);
                    assert!(block.value().len() <= maximum);
                    let context = YamlDocumentContext::new(None, &[], maximum).unwrap();
                    if let Some(identity) = context.block_scalar(&block, None) {
                        let decoded: String =
                            serde_json::from_str(identity.name().strip_prefix("str:").unwrap())
                                .unwrap();
                        assert_eq!(decoded, block.value());
                        assert!(!identity.has_unrecognized_tag());
                        assert!(identity.name().len() <= maximum);
                    }
                    admitted += 1;
                }
            }
        }
    }
    assert!(admitted > 1000);
}
