//! Positive native fixtures require clean trees and exact authored identity spans.
//! Recovery controls keep unsupported syntax distinct from qualified structures.

use tree_sitter::{Node, Parser};

fn parse(text: &str, free: bool) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    let language = if free {
        tree_sitter_cobol::FREE
    } else {
        tree_sitter_cobol::FIXED
    };
    parser.set_language(&language.into()).unwrap();
    parser.parse(text, None).unwrap()
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == kind {
            found.push(node);
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    found.sort_by_key(Node::start_byte);
    found
}

#[test]
fn free_programs_preserve_first_columns_and_code_beyond_column_seventy_two() {
    for ending in ["\n", "\r\n"] {
        let literal = format!("\"{}\"", "x".repeat(90));
        let text = format!(
            "identification division.{ending}program-id. Primary.{ending}procedure division.{ending}display {literal}.{ending}stop run.{ending}end program Primary.{ending}"
        );
        let tree = parse(&text, true);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(nodes(tree.root_node(), "program_definition").len(), 1);
        let names = nodes(tree.root_node(), "program_name");
        assert_eq!(names.len(), 2);
        for name in names {
            assert_eq!(&text[name.byte_range()], "Primary");
        }
        let strings = nodes(tree.root_node(), "string");
        assert_eq!(strings.len(), 1);
        assert_eq!(&text[strings[0].byte_range()], literal);
    }
}

#[test]
fn fixed_sequence_areas_are_profile_specific_and_original_spans_remain_exact() {
    let text = "000100 IDENTIFICATION DIVISION.\n000200 PROGRAM-ID. Primary.\n000300 PROCEDURE DIVISION.\n000400 STOP RUN.\n";
    let tree = parse(text, false);
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let names = nodes(tree.root_node(), "program_name");
    assert_eq!(names.len(), 1);
    assert_eq!(&text[names[0].byte_range()], "Primary");
    assert!(parse(text, true).root_node().has_error());
}

#[test]
fn continued_fixed_literal_keeps_every_original_segment_in_its_range() {
    for ending in ["\n", "\r\n"] {
        let literal = format!("\"first{ending}000400-    \"second\"");
        let text = format!(
            "000100 IDENTIFICATION DIVISION.{ending}000200 PROGRAM-ID. Primary.{ending}000300 PROCEDURE DIVISION.{ending}000350 DISPLAY {literal}.{ending}000500 STOP RUN.{ending}"
        );
        let tree = parse(&text, false);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let strings = nodes(tree.root_node(), "string");
        assert_eq!(strings.len(), 1);
        assert_eq!(&text[strings[0].byte_range()], literal);
    }
}

#[test]
fn authored_comment_entry_has_content_not_a_zero_width_tail() {
    let text = "identification division. program-id. Primary.\nauthor.\nwritten documentation\nprocedure division. stop run.\n";
    let tree = parse(text, true);
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let entries = nodes(tree.root_node(), "comment_entry");
    assert_eq!(entries.len(), 1);
    assert_eq!(&text[entries[0].byte_range()], "written documentation");
}

#[test]
fn comment_entry_stops_before_division_heads_with_grammar_separators() {
    for free in [false, true] {
        for ending in ["\n", "\r\n"] {
            for separator in [" ", "\t ", ", ", "; "] {
                let prefix = if free { "" } else { "       " };
                let text = format!(
                    "{prefix}identification division.{ending}{prefix}program-id. Primary.{ending}{prefix}author.{ending}{prefix}written documentation{ending}{prefix}data{separator}division.{ending}{prefix}working-storage section.{ending}{prefix}01 Stored pic x.{ending}{prefix}procedure division.{ending}{prefix}stop run.{ending}"
                );
                let tree = parse(&text, free);
                assert!(
                    !tree.root_node().has_error(),
                    "{}",
                    tree.root_node().to_sexp()
                );
                assert_eq!(
                    nodes(tree.root_node(), "data_division").len(),
                    1,
                    "division head lost for {separator:?}"
                );
                assert_eq!(nodes(tree.root_node(), "data_description").len(), 1);
                let entries = nodes(tree.root_node(), "comment_entry");
                assert_eq!(entries.len(), 1);
                assert_eq!(&text[entries[0].byte_range()], "written documentation");
            }
        }
    }
}

#[test]
fn malformed_literals_and_unsupported_function_units_remain_recovery() {
    for text in [
        "identification division. program-id. Primary. procedure division. display \"unterminated",
        "identification division. function-id. Primary. procedure division. goback.",
        "01 fragment pic x.",
        ">>source format is free\nidentification division. program-id. Primary.",
    ] {
        assert!(
            parse(text, true).root_node().has_error(),
            "unsupported syntax unexpectedly clean: {text:?}"
        );
    }
}
