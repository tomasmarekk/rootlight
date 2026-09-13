//! Native VBA syntax contracts, independent of repository identity or Office.
//! Sources are parsed only; procedure bodies and native declarations never run.

use std::ops::ControlFlow;
use tree_sitter::{InputEdit, Node, ParseOptions, Parser, Point, Tree};

const PROCEDURES: &str = "Option Explicit\n' λ😀 source marker\nPublic Function AddValues(ByVal left As Long, ByVal right As Long) As Long\nAddValues = left + right\nEnd Function\nPrivate Sub Run()\nDim total As Long\nLet total = AddValues(2, 3)\nEnd Sub\n";
const PROPERTIES: &str = "VERSION 1.0 CLASS\nBEGIN\nMultiUse = -1\nEND\nAttribute VB_Name = \"Container\"\nOption Explicit\nPrivate storedValue As Long\nPublic Property Get Value() As Long\nValue = storedValue\nEnd Property\nPublic Property Let Value(ByVal nextValue As Long)\nstoredValue = nextValue\nEnd Property\n";

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_vba::LANGUAGE.into())
        .unwrap();
    parser
}

fn point(source: &[u8], offset: usize) -> Point {
    let prefix = &source[..offset];
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |index| prefix.len() - index - 1),
    )
}

fn bounded_parse(parser: &mut Parser, source: &[u8], old: Option<&Tree>) -> Tree {
    let mut steps = 0;
    let mut progress = |_: &tree_sitter::ParseState| {
        steps += 1;
        if steps > 2_000 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    parser
        .parse_with_options(
            &mut |offset, _| source.get(offset..).unwrap_or_default(),
            old,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        )
        .expect("bounded fixture exceeded native progress watchdog")
}

fn signature(tree: &Tree, source: &[u8]) -> Vec<(String, usize, usize, bool, bool, bool)> {
    let mut pending = vec![tree.root_node()];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
        result.push((
            node.kind().to_owned(),
            node.start_byte(),
            node.end_byte(),
            node.is_named(),
            node.is_error(),
            node.is_missing(),
        ));
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result
}

fn nodes_of_kind<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![root];
    let mut found = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            found.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    found.sort_by_key(Node::start_byte);
    found
}

fn assert_statement(source: &str, kind: &str, expected: &str) {
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let statements = nodes_of_kind(tree.root_node(), kind);
    assert_eq!(statements.len(), 1, "{}", tree.root_node().to_sexp());
    assert_eq!(
        statements[0].utf8_text(source.as_bytes()).unwrap(),
        expected
    );
}

#[test]
fn explicit_let_is_an_assignment_in_block_inline_and_numbered_contexts() {
    for statement in ["Let total = 3", "lEt total = 3"] {
        for body in [
            statement.to_owned(),
            format!("If True Then {statement}"),
            format!("10 {statement}"),
        ] {
            let source = format!("Sub UpdateValue()\nDim total As Long\n{body}\nEnd Sub\n");
            assert_statement(&source, "let_statement", statement);
        }
    }
}

#[test]
fn gosub_and_return_are_control_flow_not_procedure_calls() {
    for body in [
        "GoSub LocalWork",
        "If True Then GoSub LocalWork",
        "10 GoSub LocalWork",
    ] {
        let source = format!("Sub Dispatch()\n{body}\nExit Sub\nLocalWork:\nReturn\nEnd Sub\n");
        assert_statement(&source, "gosub_statement", "GoSub LocalWork");
        assert_statement(&source, "return_statement", "Return");
        let tree = parser().parse(&source, None).unwrap();
        assert!(nodes_of_kind(tree.root_node(), "call_statement").is_empty());
    }
}

#[test]
fn string_alignment_assignments_are_not_implicit_calls() {
    for (statement, kind) in [
        ("LSet target = value", "lset_statement"),
        ("RSet target = value", "rset_statement"),
    ] {
        for body in [
            statement.to_owned(),
            format!("If True Then {statement}"),
            format!("10 {statement}"),
        ] {
            let source = format!(
                "Sub AlignText()\nDim target As String * 8\nDim value As String\n{body}\nEnd Sub\n"
            );
            assert_statement(&source, kind, statement);
        }
    }
}

#[test]
fn redim_type_clause_belongs_to_each_array_declarator() {
    let source = "Sub Allocate()\nDim values As Variant\nDim names As Variant\nReDim values(1 To 4) As Long, names(2) As String\nReDim Preserve values(1 To 8) As Long\nEnd Sub\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let declarations = nodes_of_kind(tree.root_node(), "redim_declarator");
    assert_eq!(declarations.len(), 3);
    for (node, (name, ty)) in declarations.into_iter().zip([
        ("values", "As Long"),
        ("names", "As String"),
        ("values", "As Long"),
    ]) {
        assert_eq!(
            node.child_by_field_name("name")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap(),
            name
        );
        assert_eq!(
            node.child_by_field_name("type")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap(),
            ty
        );
    }
}

#[test]
fn exported_class_and_procedure_names_retain_exact_bytes_across_line_endings() {
    for newline in ["\n", "\r\n"] {
        for (source, expectations) in [
            (
                PROCEDURES,
                vec![
                    ("function_declaration", "AddValues"),
                    ("sub_declaration", "Run"),
                ],
            ),
            (
                PROPERTIES,
                vec![
                    ("property_get_declaration", "Value"),
                    ("property_let_declaration", "Value"),
                ],
            ),
        ] {
            let source = source.replace('\n', newline);
            let tree = bounded_parse(&mut parser(), source.as_bytes(), None);
            assert!(
                !tree.root_node().has_error(),
                "{}",
                tree.root_node().to_sexp()
            );
            signature(&tree, source.as_bytes());
            for (kind, expected) in expectations {
                let declarations = nodes_of_kind(tree.root_node(), kind);
                assert_eq!(declarations.len(), 1);
                let name = declarations[0].child_by_field_name("name").unwrap();
                assert_eq!(name.utf8_text(source.as_bytes()).unwrap(), expected);
            }
        }
    }
}

#[test]
fn incomplete_declarations_do_not_become_complete_trees() {
    for source in [
        "Sub Broken(\n",
        "Function Broken() As Long\nBroken = 1\n",
        "Property Get Value() As Long\nValue = 1\n",
    ] {
        let tree = bounded_parse(&mut parser(), source.as_bytes(), None);
        assert!(tree.root_node().has_error(), "{source:?}");
        signature(&tree, source.as_bytes());
    }
}

#[test]
fn byte_damage_and_restore_match_fresh_trees_with_exact_coordinates() {
    for source in [PROCEDURES, PROPERTIES] {
        for newline in ["\n", "\r\n"] {
            let source = source.replace('\n', newline).into_bytes();
            let mut reused = parser();
            let original = bounded_parse(&mut reused, &source, None);
            assert!(!original.root_node().has_error());
            let original_signature = signature(&original, &source);
            for offset in 0..source.len() {
                let mut damaged = source.clone();
                damaged[offset] = if source[offset] == b'?' { 0xff } else { b'?' };
                let mut old = original.clone();
                old.edit(&InputEdit {
                    start_byte: offset,
                    old_end_byte: offset + 1,
                    new_end_byte: offset + 1,
                    start_position: point(&source, offset),
                    old_end_position: point(&source, offset + 1),
                    new_end_position: point(&damaged, offset + 1),
                });
                let mut incremental = bounded_parse(&mut reused, &damaged, Some(&old));
                let fresh = bounded_parse(&mut parser(), &damaged, None);
                assert_eq!(
                    signature(&incremental, &damaged),
                    signature(&fresh, &damaged),
                    "damage at {offset}"
                );
                incremental.edit(&InputEdit {
                    start_byte: offset,
                    old_end_byte: offset + 1,
                    new_end_byte: offset + 1,
                    start_position: point(&damaged, offset),
                    old_end_position: point(&damaged, offset + 1),
                    new_end_position: point(&source, offset + 1),
                });
                let restored = bounded_parse(&mut reused, &source, Some(&incremental));
                assert_eq!(
                    signature(&restored, &source),
                    original_signature,
                    "restore at {offset}"
                );
            }
        }
    }
}

#[test]
fn cancellation_then_reset_does_not_poison_the_next_document() {
    let source = format!("Sub Work()\n{}End Sub\n", "Let value = 1\n".repeat(10_000));
    let mut reused = parser();
    let mut called = false;
    let mut cancel = |_: &tree_sitter::ParseState| {
        called = true;
        ControlFlow::Break(())
    };
    let tree = reused.parse_with_options(
        &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
        None,
        Some(ParseOptions::new().progress_callback(&mut cancel)),
    );
    assert!(called && tree.is_none());
    reused.reset();
    let next = bounded_parse(&mut reused, PROCEDURES.as_bytes(), None);
    let fresh = bounded_parse(&mut parser(), PROCEDURES.as_bytes(), None);
    assert_eq!(
        signature(&next, PROCEDURES.as_bytes()),
        signature(&fresh, PROCEDURES.as_bytes())
    );
}
