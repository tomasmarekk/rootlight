//! Qualifies JSON syntax and lossless source ranges before adapter registration.
//! Object members remain ordered nodes; duplicate names must never become a map.

use tree_sitter::{InputEdit, Node, Parser, Point};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_json::LANGUAGE.into())
        .expect("compatible grammar");
    parser
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            result.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    result.sort_by_key(Node::start_byte);
    result
}

#[test]
fn json_numbers_accept_both_exponent_signs() {
    for number in [
        "0",
        "-0",
        "12",
        "0.5",
        "-12.5",
        "1e2",
        "1E-2",
        "1e+2",
        "-0.25E+03",
    ] {
        let source = format!("[{}, {{\"value\": {number}}}]", number);
        let tree = parser().parse(&source, None).expect("parse completes");
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let numbers = nodes(tree.root_node(), "number");
        assert_eq!(numbers.len(), 2);
        for node in numbers {
            assert_eq!(
                node.utf8_text(source.as_bytes()).expect("number source"),
                number
            );
        }
    }
}

#[test]
fn json_numbers_reject_incomplete_fraction_and_exponent() {
    for number in [
        "1.", "-0.", "1.e2", "1e", "1e+", "1e-", "+1", "01", "-01", ".5", "NaN", "Infinity",
    ] {
        let source = format!("[{number}]");
        let tree = parser().parse(&source, None).expect("parse completes");
        assert!(
            tree.root_node().has_error(),
            "accepted {source}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn json_unicode_escapes_are_complete_syntax_nodes() {
    for escape in [r"\u0000", r"\u0041", r"\u00e9", r"\uD83C", r"\uDF0D"] {
        let source = format!("{{\"{escape}\": \"prefix{escape}suffix\"}}");
        let tree = parser().parse(&source, None).expect("parse completes");
        assert!(!tree.root_node().has_error(), "{source}");
        let escapes = nodes(tree.root_node(), "escape_sequence");
        assert_eq!(escapes.len(), 2);
        for node in escapes {
            assert_eq!(
                node.utf8_text(source.as_bytes()).expect("escape source"),
                escape
            );
        }
    }
}

#[test]
fn json_strings_reject_raw_controls_and_malformed_escapes() {
    let malformed = [r"\u", r"\u1", r"\u123", r"\u123g", r"\x20", r"\v", r"\a"];
    for content in malformed
        .into_iter()
        .map(str::to_owned)
        .chain((0u8..=31).map(|value| char::from(value).to_string()))
    {
        let source = format!("[\"{content}\"]");
        let tree = parser().parse(&source, None).expect("parse completes");
        assert!(
            tree.root_node().has_error(),
            "accepted {source:?}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn json_duplicate_members_and_nested_arrays_keep_exact_ranges() {
    let source =
        r#"{"same":1,"same":2,"outer":[{"same":"🌍"},{"":"empty","a/b~c":"path"}],"\u0061":"a"}"#;
    let tree = parser().parse(source, None).expect("parse completes");
    assert!(!tree.root_node().has_error());
    let members = nodes(tree.root_node(), "pair");
    let keys: Vec<_> = members
        .iter()
        .map(|node| {
            node.child_by_field_name("key")
                .expect("key")
                .utf8_text(source.as_bytes())
                .expect("key source")
        })
        .collect();
    assert_eq!(
        keys,
        [
            r#""same""#,
            r#""same""#,
            r#""outer""#,
            r#""same""#,
            r#""""#,
            r#""a/b~c""#,
            r#""\u0061""#
        ]
    );
    for member in members {
        let key = member.child_by_field_name("key").expect("key");
        let value = member.child_by_field_name("value").expect("value");
        assert_eq!(member.start_byte(), key.start_byte());
        assert_eq!(member.end_byte(), value.end_byte());
        assert!(source.get(member.byte_range()).is_some());
    }
}

#[test]
fn json_string_tokens_preserve_whitespace_and_comment_like_text() {
    for content in [
        "",
        " ",
        "  ",
        "/* text */",
        "// text",
        "🌍",
        "a b",
        r"\n\t\r",
        r#"\"\\\/\b\f"#,
        r"\uD83C\uDF0D",
    ] {
        let source = format!("{{\"{content}\": \"{content}\"}}");
        let tree = parser().parse(&source, None).expect("parse completes");
        assert!(!tree.root_node().has_error(), "{source}");
        assert!(nodes(tree.root_node(), "comment").is_empty());
        let strings = nodes(tree.root_node(), "string");
        assert_eq!(strings.len(), 2);
        for node in strings {
            assert_eq!(
                node.utf8_text(source.as_bytes()).expect("string source"),
                format!("\"{content}\"")
            );
        }
    }
}

#[test]
fn upstream_editor_extensions_are_not_strict_json_validation() {
    // Keep upstream editor compatibility explicit: an error-free tree is not
    // proof of RFC 8259 validity or a decoded Unicode scalar value.
    for (source, values, comments) in [
        ("", 0, 0),
        ("{} []", 2, 0),
        ("true false", 2, 0),
        ("// note\n{}", 1, 1),
        ("/* note */[]", 1, 1),
    ] {
        let tree = parser().parse(source, None).expect("parse completes");
        let root = tree.root_node();
        assert!(!root.has_error(), "{source:?}");
        assert_eq!(nodes(root, "comment").len(), comments);
        assert_eq!(root.named_child_count(), values + comments);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct NodeStamp {
    kind: String,
    range: std::ops::Range<usize>,
    start: Point,
    end: Point,
    named: bool,
    error: bool,
    missing: bool,
    children: usize,
}

fn fingerprint(root: Node<'_>) -> Vec<NodeStamp> {
    let mut cursor = root.walk();
    let mut result = Vec::new();
    loop {
        let node = cursor.node();
        result.push(NodeStamp {
            kind: node.kind().to_owned(),
            range: node.byte_range(),
            start: node.start_position(),
            end: node.end_position(),
            named: node.is_named(),
            error: node.is_error(),
            missing: node.is_missing(),
            children: node.child_count(),
        });
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return result;
            }
        }
    }
}

fn point(source: &str, end: usize) -> Point {
    let prefix = &source[..end];
    Point::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count(),
        prefix.rsplit('\n').next().expect("last line").len(),
    )
}

#[test]
fn json_incremental_edits_match_fresh_complete_trees() {
    let source = "{\n\"same\":1e+2,\"same\":-0.5E-3,\"nested\":[{\"\\u0061\":\"🌍 \\uD83C\\uDF0D\"},null],\"tail\":true\n}";
    let mut incremental_parser = parser();
    let original = incremental_parser
        .parse(source, None)
        .expect("original parse");
    assert!(!original.root_node().has_error());
    let mut edits = 0;
    for (start, character) in source.char_indices() {
        let old_end = start + character.len_utf8();
        for replacement in ["", "\n", "🌕", "\\", "0", "\""] {
            let edited = format!("{}{}{}", &source[..start], replacement, &source[old_end..]);
            let new_end = start + replacement.len();
            let mut previous = original.clone();
            previous.edit(&InputEdit {
                start_byte: start,
                old_end_byte: old_end,
                new_end_byte: new_end,
                start_position: point(source, start),
                old_end_position: point(source, old_end),
                new_end_position: point(&edited, new_end),
            });
            let incremental = incremental_parser
                .parse(&edited, Some(&previous))
                .expect("incremental parse");
            let fresh = parser().parse(&edited, None).expect("fresh parse");
            assert_eq!(
                fingerprint(incremental.root_node()),
                fingerprint(fresh.root_node()),
                "edit at {start} -> {replacement:?}"
            );
            edits += 1;
        }
    }
    assert_eq!(edits, source.chars().count() * 6);
    incremental_parser.reset();
    let restored = incremental_parser.parse(source, None).expect("reset parse");
    assert_eq!(
        fingerprint(restored.root_node()),
        fingerprint(original.root_node())
    );
}
