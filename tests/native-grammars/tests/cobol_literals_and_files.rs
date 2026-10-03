//! Authored native regressions for prefixed literals and file organization syntax.
//! Clean trees and query captures retain original bytes and physical coordinates;
//! these checks establish neither literal decoding nor file/runtime semantics.

use std::ops::{ControlFlow, Range};

use tree_sitter::{
    Language, Node, ParseOptions, Parser, Point, Query, QueryCursor, StreamingIterator, Tree,
};

const QUERY: &str = include_str!("../../../crates/rootlight-adapter-treesitter/queries/cobol.scm");

#[derive(Clone, Copy, Debug)]
enum Profile {
    Fixed,
    Free,
}

impl Profile {
    fn language(self) -> Language {
        match self {
            Self::Fixed => tree_sitter_cobol::FIXED.into(),
            Self::Free => tree_sitter_cobol::FREE.into(),
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Fixed => "       ",
            Self::Free => "",
        }
    }
}

fn source(lines: &[&str], profile: Profile, ending: &str, final_newline: bool) -> String {
    let mut text = String::new();
    for (index, line) in lines.iter().enumerate() {
        text.push_str(profile.prefix());
        text.push_str(line);
        if final_newline || index + 1 < lines.len() {
            text.push_str(ending);
        }
    }
    text
}

fn parse(text: &str, profile: Profile) -> Tree {
    let mut parser = Parser::new();
    parser.set_language(&profile.language()).unwrap();
    let mut steps = 0_usize;
    let mut watchdog = |_: &tree_sitter::ParseState| {
        steps = steps.saturating_add(1);
        if steps > 2_000 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    parser
        .parse_with_options(
            &mut |offset, _| text.as_bytes().get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut watchdog)),
        )
        .expect("authored syntax fixture exceeded progress watchdog")
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut found = Vec::new();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if node.kind() == kind {
            found.push(node);
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                found.sort_by_key(Node::start_byte);
                return found;
            }
        }
    }
}

fn point(text: &str, offset: usize) -> Point {
    let prefix = text.as_bytes().get(..offset).unwrap();
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn clean(tree: &Tree, text: &str) {
    assert_eq!(tree.root_node().kind(), "start");
    assert!(
        !tree.root_node().has_error(),
        "authored source {text:?}: {}",
        tree.root_node().to_sexp()
    );
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        assert!(!node.is_error() && !node.is_missing());
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= text.len());
        assert_eq!(node.start_position(), point(text, node.start_byte()));
        assert_eq!(node.end_position(), point(text, node.end_byte()));
        assert_eq!(
            node.utf8_text(text.as_bytes()).unwrap(),
            text.get(node.byte_range()).unwrap()
        );
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

fn recovery(tree: &Tree, text: &str) {
    assert!(
        tree.root_node().has_error(),
        "malformed authored source unexpectedly clean {text:?}: {}",
        tree.root_node().to_sexp()
    );
    let mut cursor = tree.walk();
    loop {
        if cursor.node().is_error() || cursor.node().is_missing() {
            return;
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                // Native serialization exposes invisible missing leaves that
                // neither cursor traversal nor child() can return as nodes.
                let sexp = tree.root_node().to_sexp();
                assert!(
                    sexp.contains("(MISSING "),
                    "error-bearing source has no recovery witness {text:?}: {sexp}"
                );
                return;
            }
        }
    }
}

fn exact(node: Node<'_>, text: &str, expected: Range<usize>) {
    assert_eq!(node.byte_range(), expected);
    assert_eq!(node.start_position(), point(text, expected.start));
    assert_eq!(node.end_position(), point(text, expected.end));
    assert_eq!(
        node.utf8_text(text.as_bytes()).unwrap(),
        text.get(expected).unwrap()
    );
}

fn string_captures<'tree>(tree: &'tree Tree, text: &str, profile: Profile) -> Vec<Node<'tree>> {
    let query = Query::new(&profile.language(), QUERY).unwrap();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), text.as_bytes());
    let mut found = Vec::new();
    while let Some(matched) = matches.next() {
        for capture in matched.captures {
            let index = usize::try_from(capture.index).unwrap();
            if query.capture_names()[index] == "string" {
                found.push(capture.node);
            }
        }
    }
    found.sort_by_key(Node::start_byte);
    found
}

fn literal_source(literal: &str, profile: Profile, ending: &str, final_newline: bool) -> String {
    source(
        &[
            "identification division.",
            "program-id. Literals.",
            "data division.",
            "working-storage section.",
            &format!("01 Stored pic x(4) value {literal}."),
            "procedure division.",
            &format!("display {literal}."),
            "stop run.",
            "end program Literals.",
        ],
        profile,
        ending,
        final_newline,
    )
}

fn exact_prefixed_literals(text: &str, literal: &str, profile: Profile) {
    let tree = parse(text, profile);
    clean(&tree, text);
    let written: Vec<_> = text
        .match_indices(literal)
        .map(|(start, _)| start..start + literal.len())
        .collect();
    assert_eq!(written.len(), 2);
    let native = nodes(tree.root_node(), "x_string");
    let captures = string_captures(&tree, text, profile);
    assert_eq!(native.len(), 2);
    assert_eq!(captures.len(), 2);
    for ((node, capture), expected) in native.into_iter().zip(captures).zip(written) {
        exact(node, text, expected.clone());
        exact(capture, text, expected);
        assert_eq!(capture.kind(), "x_string");
        assert_eq!(capture.id(), node.id());
    }
}

#[test]
fn x_literal_prefix_case_and_quote_style_keep_raw_spans_and_query_captures() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for prefix in ['x', 'X'] {
                    for quote in ['\'', '"'] {
                        let literal = format!("{prefix}{quote}0aF1{quote}");
                        let text = literal_source(&literal, profile, ending, final_newline);
                        exact_prefixed_literals(&text, &literal, profile);
                    }
                }
            }
        }
    }
}

#[test]
fn prefixed_literal_payload_remains_lexical_without_hexadecimal_validation() {
    // The retained production accepts arbitrary quoted payloads, not decoded hex.
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for prefix in ['x', 'X'] {
                    for quote in ['\'', '"'] {
                        let literal = format!("{prefix}{quote}not-hex{quote}");
                        let text = literal_source(&literal, profile, ending, final_newline);
                        exact_prefixed_literals(&text, &literal, profile);
                    }
                }
            }
        }
    }
}

#[test]
fn malformed_literal_prefixes_and_nonclosing_quotes_remain_recovery() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for literal in [
                    "x'0aF1", "X'0aF1", "x\"0aF1", "X\"0aF1", "x'0aF1\"", "X\"0aF1'", "xx'0aF1'",
                    "x '0aF1'",
                ] {
                    let text = literal_source(literal, profile, ending, final_newline);
                    recovery(&parse(&text, profile), &text);
                }
            }
        }
    }
}

#[test]
fn ordinary_x_storage_operand_does_not_become_a_prefixed_literal() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let text = source(
                    &[
                        "identification division.",
                        "program-id. Operands.",
                        "data division.",
                        "working-storage section.",
                        "01 x pic x.",
                        "procedure division.",
                        "display x '0aF1'.",
                        "stop run.",
                        "end program Operands.",
                    ],
                    profile,
                    ending,
                    final_newline,
                );
                let tree = parse(&text, profile);
                clean(&tree, &text);
                assert!(nodes(tree.root_node(), "x_string").is_empty());
                let captures = string_captures(&tree, &text, profile);
                assert_eq!(captures.len(), 1);
                assert_eq!(captures[0].kind(), "string");
                let start = text.find("'0aF1'").unwrap();
                exact(captures[0], &text, start..start + "'0aF1'".len());
                let operands = nodes(tree.root_node(), "qualified_word");
                assert_eq!(operands.len(), 1);
                let start = text.find("display x").unwrap() + "display ".len();
                exact(operands[0], &text, start..start + 1);
            }
        }
    }
}

fn organization_source(
    clause: &str,
    profile: Profile,
    ending: &str,
    final_newline: bool,
) -> String {
    source(
        &[
            "identification division.",
            "program-id. Organization.",
            "environment division.",
            "input-output section.",
            "file-control.",
            "select Records",
            "assign to 'records.dat'",
            &format!("{clause}."),
            "procedure division.",
            "stop run.",
            "end program Organization.",
        ],
        profile,
        ending,
        final_newline,
    )
}

fn exact_organization(text: &str, clause: &str, profile: Profile) {
    let tree = parse(text, profile);
    clean(&tree, text);
    let selections = nodes(tree.root_node(), "select_statement");
    let organizations = nodes(tree.root_node(), "organization_clause");
    assert_eq!(selections.len(), 1);
    assert_eq!(organizations.len(), 1);
    let start = text.find(clause).unwrap();
    exact(organizations[0], text, start..start + clause.len());
    assert_eq!(organizations[0].parent().unwrap().id(), selections[0].id());
    let select_start = text.find("select Records").unwrap();
    exact(selections[0], text, select_start..start + clause.len() + 1);
    let file_name = selections[0].child_by_field_name("file_name").unwrap();
    let name_start = select_start + "select ".len();
    exact(file_name, text, name_start..name_start + "Records".len());
    assert_eq!(file_name.kind(), "WORD");
    let captures = string_captures(&tree, text, profile);
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].kind(), "string");
    let literal_start = text.find("'records.dat'").unwrap();
    exact(
        captures[0],
        text,
        literal_start..literal_start + "'records.dat'".len(),
    );
}

#[test]
fn line_sequential_organization_keeps_written_case_and_optional_keywords() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for clause in [
                    "organization is line sequential",
                    "ORGANIZATION IS LINE SEQUENTIAL",
                    "OrGaNiZaTiOn Is LiNe SeQuEnTiAl",
                    "organization line sequential",
                    "line sequential",
                ] {
                    let text = organization_source(clause, profile, ending, final_newline);
                    exact_organization(&text, clause, profile);
                }
            }
        }
    }
}

#[test]
fn existing_organization_alternatives_keep_exact_native_clause_ownership() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for organization in [
                    "sequential",
                    "record sequential",
                    "record binary sequential",
                    "indexed",
                    "relative",
                ] {
                    for prefix in ["organization is ", "organization ", ""] {
                        let clause = format!("{prefix}{organization}");
                        let text = organization_source(&clause, profile, ending, final_newline);
                        exact_organization(&text, &clause, profile);
                    }
                    let clause = format!("ORGANIZATION IS {}", organization.to_ascii_uppercase());
                    let text = organization_source(&clause, profile, ending, final_newline);
                    exact_organization(&text, &clause, profile);
                }
            }
        }
    }
}

#[test]
fn line_sequential_split_across_physical_lines_retains_all_original_bytes() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let clause = format!(
                    "organization is{ending}{}line{ending}{}sequential",
                    profile.prefix(),
                    profile.prefix(),
                );
                let text = organization_source(&clause, profile, ending, final_newline);
                exact_organization(&text, &clause, profile);
            }
        }
    }
}

#[test]
fn truncated_nonkeyword_and_unclosed_organization_clauses_remain_recovery() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for clause in [
                    "organization is",
                    "organization is line",
                    "organization is line indexed",
                    "organization is line relative",
                    "organization is linear sequential",
                    "organization is line sequentialish",
                    "organization is record binary",
                    "organization is line sequential extra",
                ] {
                    let text = organization_source(clause, profile, ending, final_newline);
                    recovery(&parse(&text, profile), &text);
                }
                let clause = "organization is line sequential";
                let full = organization_source(clause, profile, ending, final_newline);
                let start = full.find(clause).unwrap();
                let mut truncated = full.get(..start + clause.len()).unwrap().to_owned();
                if final_newline {
                    truncated.push_str(ending);
                }
                recovery(&parse(&truncated, profile), &truncated);
            }
        }
    }
}
