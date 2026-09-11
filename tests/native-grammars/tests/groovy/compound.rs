//! Compound statements must retain source-bound branch and declaration ownership.
//! These finite authored inputs are parsed only, never evaluated as Groovy code.

use tree_sitter::{Node, Parser, Point, Tree};

fn parse(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
        result.push(node);
    }
    result
}

fn point(source: &str, byte: usize) -> Point {
    let prefix = &source[..byte];
    Point::new(
        prefix.bytes().filter(|b| *b == b'\n').count(),
        prefix
            .rfind('\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn valid(source: &str) -> Tree {
    let tree = parse(source);
    assert!(
        !tree.root_node().has_error(),
        "{source}\n{}",
        tree.root_node().to_sexp()
    );
    for node in nodes(tree.root_node()) {
        assert!(source.get(node.byte_range()).is_some());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
    }
    tree
}

fn exact(tree: &Tree, source: &str, kind: &str, text: &str) {
    assert!(
        nodes(tree.root_node())
            .iter()
            .any(|n| n.kind() == kind && n.utf8_text(source.as_bytes()).unwrap() == text),
        "missing {kind} {text:?}: {}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn classic_switch_preserves_branch_statements() {
    let source = "switch (value) {\ncase 1:\ndef first = 'λ😀';\nconsume(first);\nbreak;\ndefault:\ndef second = 2\nconsume(second)\n}\n";
    let tree = valid(source);
    exact(
        &tree,
        source,
        "local_variable_declaration",
        "def first = 'λ😀'",
    );
    exact(
        &tree,
        source,
        "local_variable_declaration",
        "def second = 2",
    );
    let case = nodes(tree.root_node())
        .into_iter()
        .find(|n| n.kind() == "switch_case")
        .unwrap();
    assert_eq!(
        nodes(case)
            .iter()
            .filter(|n| n.kind() == "local_variable_declaration")
            .count(),
        1
    );
    assert_eq!(
        nodes(case)
            .iter()
            .filter(|n| n.kind() == "break_statement")
            .count(),
        1
    );
    assert!(
        !case
            .utf8_text(source.as_bytes())
            .unwrap()
            .contains("default")
    );
}

#[test]
fn switch_rejects_unseparated_declarations() {
    for source in [
        "switch (value) { case 1: def first = 1 def second = 2 }",
        "switch (value) { default: def first = 1 def second = 2 }",
    ] {
        assert!(parse(source).root_node().has_error(), "accepted {source}");
    }
}

#[test]
fn switch_fallthrough_labels_preserve_one_body() {
    let source = "switch (value) {\ncase 1:\ncase 2:\nconsume()\nbreak\ndefault:\nfinish()\n}\n";
    let tree = valid(source);
    assert_eq!(
        nodes(tree.root_node())
            .iter()
            .filter(|n| n.kind() == "switch_case")
            .count(),
        2
    );
    exact(&tree, source, "method_invocation", "consume()");
    exact(&tree, source, "method_invocation", "finish()");
}

#[test]
fn arrow_switch_separates_expression_and_block_branches() {
    let source = "def result = switch (value) {\ncase 1 -> first();\ncase 2 -> { def local = 2; yield local }\ndefault -> last()\n}\n";
    let tree = valid(source);
    exact(&tree, source, "method_invocation", "first()");
    exact(&tree, source, "yield_statement", "yield local");
    exact(&tree, source, "method_invocation", "last()");
}

#[test]
fn enum_members_have_real_separators() {
    let source = "enum Mode {\nFAST,\nSLOW;\nint code = 1;\nint read() { code }\nint twice() { code * 2 }\n}\n";
    let tree = valid(source);
    exact(&tree, source, "enum_constant", "FAST");
    exact(&tree, source, "enum_constant", "SLOW");
    exact(&tree, source, "field_declaration", "int code = 1");
    exact(&tree, source, "method_declaration", "int read() { code }");
    exact(
        &tree,
        source,
        "method_declaration",
        "int twice() { code * 2 }",
    );
}

#[test]
fn enum_rejects_unseparated_members() {
    assert!(
        parse("enum Mode { FAST; int first = 1 int second = 2 }")
            .root_node()
            .has_error()
    );
}

#[test]
fn enum_constants_allow_linebreak_before_comma_and_trailing_comma() {
    valid("enum Mode {\nFAST\n,\nSLOW\n,\n}\n");
}

#[test]
fn enum_members_can_follow_newline_without_semicolon() {
    let source = "enum Mode {\nFAST, SLOW\nint code = 1\nint read() { code }\n}\n";
    let tree = valid(source);
    exact(&tree, source, "field_declaration", "int code = 1");
    exact(&tree, source, "method_declaration", "int read() { code }");
}

#[test]
fn member_continuation_across_comments_keeps_comments_visible() {
    for gap in [
        "\n// note\n",
        "\n/* λ😀 */\n",
        " /* note */\n",
        "\n/** note */\n",
    ] {
        let expression = format!("value{gap}.toString()");
        let source = format!("def result = {expression}\n");
        let tree = valid(&source);
        exact(&tree, &source, "method_invocation", &expression);
        assert!(
            nodes(tree.root_node())
                .iter()
                .any(|n| n.kind().ends_with("comment"))
        );
    }
}

#[test]
fn enum_constructor_arguments_do_not_become_parameters() {
    let source = "enum Mode { FAST(1), SLOW(2); int code; Mode(int value) { code = value } }\n";
    let tree = valid(source);
    exact(&tree, source, "enum_constant", "FAST(1)");
    exact(&tree, source, "enum_constant", "SLOW(2)");
    exact(
        &tree,
        source,
        "constructor_declaration",
        "Mode(int value) { code = value }",
    );
}

#[test]
fn enum_without_constants_retains_members() {
    for source in [
        "enum Mode { int code = 1\nint read() { code } }\n",
        "enum Mode {\n}\n",
    ] {
        valid(source);
    }
    assert!(
        parse("enum Mode { ; int code = 1\nint read() { code } }\n")
            .root_node()
            .has_error()
    );
}

#[test]
fn compound_incremental_edits_match_fresh_ranges() {
    use tree_sitter::InputEdit;
    for (source, before, after) in [
        (
            "enum Mode { FAST, SLOW; int code = 1; int read() { code } }\n",
            ";",
            "\r\n",
        ),
        (
            "enum Mode { FAST, SLOW; int code = 1; int read() { code } }\n",
            "FAST, SLOW",
            "FAST\r\n,\r\nSLOW\r\n,",
        ),
        (
            "switch (value) { case 1: def text = 'λ😀'; consume(text); break; default: finish() }\n",
            "λ😀",
            "δ🌍!",
        ),
        (
            "switch (value) { case 1: consume(); break; default: finish() }\n",
            "case 1:",
            "case 1:\r\ncase 2:\r\n",
        ),
    ] {
        let start = source.find(before).unwrap();
        let changed = source.replacen(before, after, 1);
        let mut parser = Parser::new();
        parser
            .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
            .unwrap();
        let mut old = valid(source);
        old.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + before.len(),
            new_end_byte: start + after.len(),
            start_position: point(source, start),
            old_end_position: point(source, start + before.len()),
            new_end_position: point(&changed, start + after.len()),
        });
        let incremental = parser.parse(&changed, Some(&old)).unwrap();
        let fresh = valid(&changed);
        let signature = |tree: &Tree| {
            nodes(tree.root_node())
                .into_iter()
                .map(|n| {
                    (
                        n.kind().to_owned(),
                        n.byte_range(),
                        n.start_position(),
                        n.end_position(),
                        n.is_missing(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(signature(&incremental), signature(&fresh));
    }
}

#[test]
fn malformed_compound_reset_preserves_following_parse() {
    let source = "enum Mode { FAST; int read() { 1 } }\n";
    let expected = valid(source).root_node().to_sexp();
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    for damaged in [
        "enum Mode { FAST,",
        "switch (value) { case 1:",
        "switch (value) { default: def a = 1 def b = 2 }",
    ] {
        assert!(parser.parse(damaged, None).unwrap().root_node().has_error());
        parser.reset();
        assert_eq!(
            parser.parse(source, None).unwrap().root_node().to_sexp(),
            expected
        );
    }
}
