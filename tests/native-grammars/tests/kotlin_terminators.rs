//! Kotlin class-member terminators preserve the written closing brace and source ranges.
//! These native syntax checks do not execute Kotlin or establish compiler semantics.

use std::ops::ControlFlow;
use tree_sitter::{Node, ParseOptions, Parser, Point, Tree};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
        .unwrap();
    parser
}

fn bounded_parse(parser: &mut Parser, source: &[u8]) -> Tree {
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
            &mut |offset, _| source.get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut watchdog)),
        )
        .expect("native terminator fixture exceeded progress watchdog")
}

fn point(source: &[u8], offset: usize) -> Point {
    let prefix = &source[..offset];
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn fingerprint(tree: &Tree, source: &[u8]) -> Vec<(String, usize, usize, bool, bool, bool)> {
    let mut pending: Vec<Node<'_>> = vec![tree.root_node()];
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
    result.sort();
    result
}

fn assert_written_node(
    fingerprint: &[(String, usize, usize, bool, bool, bool)],
    source: &[u8],
    kind: &str,
    written: &str,
) {
    assert!(
        fingerprint.iter().any(|(actual, start, end, _, _, _)| {
            actual == kind && source.get(*start..*end) == Some(written.as_bytes())
        }),
        "missing exact {kind} {written:?}: {fingerprint:?}"
    );
}

#[test]
fn final_class_and_enum_members_need_no_written_separator() {
    let cases = [
        ("class Sample { fun run(): Unit {} }", "block", "{}"),
        ("class Sample { val value: Int = 1 }", "number_literal", "1"),
        ("class Sample { var value: Int = 1 }", "number_literal", "1"),
        (
            "enum class Sample { Entry; fun run(): Unit {} }",
            "block",
            "{}",
        ),
        (
            "class Sample { fun run(): Unit {}/* tail */ }",
            "block_comment",
            "/* tail */",
        ),
        (
            "class Sample { fun run(): Unit {} // tail\n}",
            "line_comment",
            "// tail",
        ),
        (
            "class Outer { class Inner { fun run(): Unit {} } }",
            "block",
            "{}",
        ),
        (
            "class Sample { fun first(): Unit {}; fun last(): Unit {} }",
            "block",
            "{}",
        ),
    ];
    for ending in ["\n", "\r\n"] {
        for final_newline in [false, true] {
            for (authored, kind, written) in cases {
                let mut text = authored.replace('\n', ending);
                if final_newline {
                    text.push_str(ending);
                }
                let tree = bounded_parse(&mut parser(), text.as_bytes());
                assert!(
                    !tree.root_node().has_error(),
                    "{text:?}: {}",
                    tree.root_node().to_sexp()
                );
                let evidence = fingerprint(&tree, text.as_bytes());
                // The retained //.* token includes the authored CR before LF.
                let written = if kind == "line_comment" && ending == "\r\n" {
                    format!("{written}\r")
                } else {
                    written.to_owned()
                };
                assert_written_node(&evidence, text.as_bytes(), kind, &written);
                assert!(
                    evidence
                        .iter()
                        .any(|(kind, _, _, _, _, _)| kind == "class_declaration")
                );
                assert!(
                    evidence
                        .iter()
                        .all(|(_, _, _, _, error, missing)| !error && !missing)
                );
            }
        }
    }
}

#[test]
fn ordinary_statement_and_member_continuations_keep_their_native_shape() {
    let cases = [
        ("class Empty {}", "class_body", "{}"),
        (
            "fun run(): Unit { val value = 1 }",
            "block",
            "{ val value = 1 }",
        ),
        (
            "class Sample { val value = first\n + other }",
            "binary_expression",
            "first\n + other",
        ),
        (
            "class Sample { val value: Int\n get() = 1 }",
            "getter",
            "get() = 1",
        ),
        ("class Sample { fun run(): Unit {}\n}", "block", "{}"),
        ("class Sample { fun run(): Unit {}; }", "block", "{}"),
    ];
    for ending in ["\n", "\r\n"] {
        for (authored, kind, written) in cases {
            let text = format!("{}{ending}", authored.replace('\n', ending));
            let tree = bounded_parse(&mut parser(), text.as_bytes());
            assert!(
                !tree.root_node().has_error(),
                "{text:?}: {}",
                tree.root_node().to_sexp()
            );
            let evidence = fingerprint(&tree, text.as_bytes());
            assert_written_node(
                &evidence,
                text.as_bytes(),
                kind,
                &written.replace('\n', ending),
            );
        }
    }
}

#[test]
fn missing_member_names_types_and_bodies_still_require_native_recovery() {
    let cases = [
        "class Broken { fun (): Unit {} }",
        "class Broken { fun run(): {} }",
        "class Broken { fun run(): Unit { }",
        "class Broken { val value: = 1 }",
        "class Broken { val value = }",
        "enum class Broken { Entry; fun run(): {} }",
    ];
    for ending in ["\n", "\r\n"] {
        for authored in cases {
            let text = format!("{authored}{ending}");
            let tree = bounded_parse(&mut parser(), text.as_bytes());
            assert!(
                tree.root_node().has_error(),
                "malformed member was accepted: {text:?}: {}",
                tree.root_node().to_sexp()
            );
            let _ = fingerprint(&tree, text.as_bytes());
        }
    }
}

#[test]
fn cancellation_then_reset_keeps_exact_final_member_ranges() {
    for ending in ["\n", "\r\n"] {
        let large = format!("class Sample {{ fun run(): Unit {{}} }}{ending}").repeat(400);
        let mut reused = parser();
        let mut called = false;
        let mut cancel = |_: &tree_sitter::ParseState| {
            called = true;
            ControlFlow::Break(())
        };
        let cancelled = reused.parse_with_options(
            &mut |offset, _| large.as_bytes().get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut cancel)),
        );
        assert!(called && cancelled.is_none());
        reused.reset();
        let text = format!("class Secondary {{ val value: Int = 1 }}{ending}");
        let actual = bounded_parse(&mut reused, text.as_bytes());
        let fresh = bounded_parse(&mut parser(), text.as_bytes());
        assert!(!actual.root_node().has_error());
        assert_eq!(
            fingerprint(&actual, text.as_bytes()),
            fingerprint(&fresh, text.as_bytes())
        );
    }
}
