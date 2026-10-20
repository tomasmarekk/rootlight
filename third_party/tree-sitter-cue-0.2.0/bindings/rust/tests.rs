//! Exercise retained CUE tables with source-backed and incremental parser input.
//! These native controls establish syntax behavior rather than evaluated meaning.

use tree_sitter::{InputEdit, Parser, Point};

const SOURCE: &str = include_str!("../../tests/fixtures/environment.cue");

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser.set_language(&super::LANGUAGE.into()).unwrap();
    parser
}

#[test]
fn original_fields_and_raw_strings_parse_with_unchanged_tables() {
    let mut parser = parser();
    let tree = parser.parse(SOURCE, None).unwrap();
    assert!(!tree.root_node().has_error());
    let mut cursor = tree.walk();
    let mut exact = 0;
    loop {
        let node = cursor.node();
        if node.kind() == "identifier" {
            for (start, end, written) in [
                (58, 69, "UserDefined"),
                (93, 101, "buildkit"),
                (108, 115, "address"),
                (152, 157, "hosts"),
            ] {
                if node.start_byte() == start && node.end_byte() == end {
                    assert_eq!(node.utf8_text(SOURCE.as_bytes()).unwrap(), written);
                    assert_eq!(node.parent().unwrap().kind(), "label");
                    assert_eq!(node.parent().unwrap().parent().unwrap().kind(), "field");
                    exact += 1;
                }
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                assert_eq!(exact, 4);
                let raw = "package sample\nraw: #\"written\"#\nnested: #\"prefix \\#( #\"inner\"# ) suffix\"#\n";
                assert!(!parser.parse(raw, None).unwrap().root_node().has_error());
                return;
            }
        }
    }
}

#[test]
fn incremental_edits_preserve_fresh_tree_and_malformed_state_resets() {
    let mut parser = parser();
    let source = "package sample\nfield: {nested: string}\n";
    let mut tree = parser.parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    let suffix = "extra: ##\"raw value\"##\n";
    let updated = format!("{source}{suffix}");
    let end = tree.root_node().end_position();
    tree.edit(&InputEdit {
        start_byte: source.len(),
        old_end_byte: source.len(),
        new_end_byte: updated.len(),
        start_position: end,
        old_end_position: end,
        new_end_position: Point {
            row: end.row + 1,
            column: 0,
        },
    });
    let incremental = parser.parse(&updated, Some(&tree)).unwrap();
    let fresh = parser.parse(&updated, None).unwrap();
    assert!(!incremental.root_node().has_error());
    assert_eq!(
        incremental.root_node().to_sexp(),
        fresh.root_node().to_sexp()
    );
    assert_eq!(
        incremental.root_node().byte_range(),
        fresh.root_node().byte_range()
    );
    assert!(
        parser
            .parse("package sample\na: #\"unfinished\n", None)
            .unwrap()
            .root_node()
            .has_error()
    );
    assert!(!parser.parse(source, None).unwrap().root_node().has_error());
}
