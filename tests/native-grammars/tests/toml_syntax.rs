//! Qualifies native TOML syntax independently of structural adapter lowering.
//! Parse nodes retain source spelling; they do not resolve dotted-key tables.

use tree_sitter::{InputEdit, Node, Parser, Point};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_toml_ng::LANGUAGE.into())
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

fn assert_value(value: &str, kind: &str) {
    let source = format!("value = {value}\n");
    let tree = parser().parse(&source, None).expect("parse completes");
    assert!(
        !tree.root_node().has_error(),
        "{source:?}: {}",
        tree.root_node().to_sexp()
    );
    let values = nodes(tree.root_node(), kind);
    assert_eq!(values.len(), 1, "{source:?}");
    assert_eq!(values[0].utf8_text(source.as_bytes()).unwrap(), value);
}

#[test]
fn toml_11_string_escapes_keep_exact_source() {
    for escape in [
        r"\e",
        r"\x00",
        r"\x1B",
        r"\xe9",
        r"\xFF",
        r"\u00e9",
        r"\U0001F30D",
    ] {
        for quote in ["\"", "\"\"\""] {
            let source = format!("value = {quote}before{escape}after{quote}\n");
            let tree = parser().parse(&source, None).expect("parse completes");
            assert!(!tree.root_node().has_error(), "{source:?}");
            let escapes = nodes(tree.root_node(), "escape_sequence");
            assert_eq!(escapes.len(), 1);
            assert_eq!(escapes[0].utf8_text(source.as_bytes()).unwrap(), escape);
        }
    }
}

#[test]
fn toml_11_inline_tables_allow_newlines_comments_and_trailing_comma() {
    for value in [
        "{ first = 1, }",
        "{\nfirst = 1\n}",
        "{ # start\nfirst = 1, # end\n}",
        "{\r\nfirst = 1\r\n,\r\nsecond = 2,\r\n}",
        "{\n# empty\n}",
    ] {
        assert_value(value, "inline_table");
    }
    let source =
        "value = {\n child = {\n name = '🌍',\n },\n list = [{ x = 1, }, { x = 2, }],\n}\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    assert_eq!(nodes(tree.root_node(), "inline_table").len(), 4);
    assert_eq!(nodes(tree.root_node(), "pair").len(), 6);
}

#[test]
fn toml_11_times_allow_omitted_seconds() {
    for (value, kind) in [
        ("07:32", "local_time"),
        ("07:32:00.123456", "local_time"),
        ("2024-02-29T07:32", "local_date_time"),
        ("2024-02-29 07:32Z", "offset_date_time"),
        ("2024-02-29t07:32-07:00", "offset_date_time"),
        ("2024-02-29T07:32+01:00", "offset_date_time"),
        ("2024-02-29T07:32:59.5z", "offset_date_time"),
    ] {
        assert_value(value, kind);
    }
}

#[test]
fn toml_multiline_continuations_allow_space_before_newline() {
    for newline in ["\n", "\r\n"] {
        for whitespace in ["", " ", "\t", " \t "] {
            let value = format!("\"\"\"before\\{whitespace}{newline} \t{newline} after\"\"\"");
            assert_value(&value, "string");
        }
    }
}

#[test]
fn toml_string_delimiters_preserve_literal_content_and_tabs() {
    for value in [
        "\"\"",
        "''",
        "\" \t # [not a table] \"",
        "' \t # literal '",
        "\"\"\"\n🌍 \" quoted \"\" text\n\"\"\"",
        "'''\r\n🌍 ' quoted '' text\r\n'''",
        "\"\"\"\"one quote\"\"\"\"",
        "\"\"\"\"\"two quotes\"\"\"\"\"",
        "''''one quote''''",
        "'''''two quotes'''''",
    ] {
        assert_value(value, "string");
    }
}

#[test]
fn toml_keys_tables_and_array_elements_keep_source_boundaries() {
    let source = "\"\" = 1\n' ' = 2\nfruit . \"a.b\" . color = 'red'\n[settings.'a.b']\nname = '🌍'\n[[items]]\nname = 'first'\n[items.detail]\nid = 1\n[[items]]\nname = 'second'\n";
    let tree = parser().parse(source, None).unwrap();
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let pairs = nodes(root, "pair");
    let keys: Vec<_> = pairs
        .iter()
        .map(|node| {
            node.named_child(0)
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap()
        })
        .collect();
    assert_eq!(
        keys,
        [
            "\"\"",
            "' '",
            "fruit . \"a.b\" . color",
            "name",
            "name",
            "id",
            "name"
        ]
    );
    assert_eq!(nodes(root, "table").len(), 2);
    let elements = nodes(root, "table_array_element");
    assert_eq!(elements.len(), 2);
    for (element, name) in elements.iter().zip(["first", "second"]) {
        let text = element.utf8_text(source.as_bytes()).unwrap();
        assert!(text.starts_with("[[items]]"));
        assert!(text.contains(&format!("name = '{name}'")));
        assert!(!text.contains("[items.detail]"));
    }
    for pair in pairs {
        let key = pair.named_child(0).unwrap();
        let value = pair.named_child(1).unwrap();
        assert_eq!(pair.start_byte(), key.start_byte());
        assert!(pair.end_byte() >= value.end_byte());
        assert!(source.get(pair.byte_range()).is_some());
    }
}

#[test]
fn toml_rejects_malformed_lexical_values() {
    for value in [
        r#""\x0""#,
        r#""\xGG""#,
        r#""\u123""#,
        r#""\v""#,
        "07:32.5",
        "24:00",
        "07:60",
        "01",
        "1.",
        "1e+",
        "0x_1",
        "{a = 1 b = 2}",
        "{,}",
        "{a = 1,,}",
        "[1,,]",
    ] {
        let source = format!("value = {value}\n");
        let tree = parser().parse(&source, None).unwrap();
        assert!(tree.root_node().has_error(), "accepted {source:?}");
    }
    for control in (0u8..=31).chain([127]).filter(|byte| *byte != b'\t') {
        for quote in ["\"", "'"] {
            let source = format!("value = {quote}{}{quote}\n", char::from(control));
            let tree = parser().parse(&source, None).unwrap();
            assert!(tree.root_node().has_error(), "accepted {source:?}");
        }
    }
}

#[test]
fn toml_syntax_tree_is_not_semantic_validation() {
    // Syntax consumers must not mistake recovered duplicate declarations or
    // lexically shaped dates/escapes for a validated, decoded TOML data model.
    for source in [
        "key = 1\nkey = 2\n",
        "[a]\n[a]\n",
        "date = 2023-02-29\n",
        "key = \"\\uD800\"\n",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(!tree.root_node().has_error(), "{source:?}");
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    kind: String,
    range: std::ops::Range<usize>,
    start: Point,
    end: Point,
    flags: (bool, bool, bool),
    children: usize,
}

fn fingerprint(root: Node<'_>) -> Vec<Stamp> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        result.push(Stamp {
            kind: node.kind().to_owned(),
            range: node.byte_range(),
            start: node.start_position(),
            end: node.end_position(),
            flags: (node.is_named(), node.is_error(), node.is_missing()),
            children: node.child_count(),
        });
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result
}

fn point(source: &str, end: usize) -> Point {
    let prefix = &source[..end];
    Point::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count(),
        prefix.rsplit('\n').next().unwrap().len(),
    )
}

#[test]
fn toml_incremental_edits_match_full_trees() {
    let source = "[data.\"🌍\"]\nvalue = \"\"\"a\"\"b\nend\"\"\"\nliteral = '''a''b\nend'''\n[[items]]\nx = [1, 2, { id = 3 }]\n";
    let mut incremental_parser = parser();
    let original = incremental_parser.parse(source, None).unwrap();
    assert!(!original.root_node().has_error());
    let mut edits = 0;
    for (start, character) in source.char_indices() {
        let old_end = start + character.len_utf8();
        for replacement in ["", "\n", "🌕", "\\", "'", "\""] {
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
            let incremental = incremental_parser.parse(&edited, Some(&previous)).unwrap();
            let fresh = parser().parse(&edited, None).unwrap();
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
    let restored = incremental_parser.parse(source, None).unwrap();
    assert_eq!(
        fingerprint(restored.root_node()),
        fingerprint(original.root_node())
    );
}
