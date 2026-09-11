//! Statement boundaries must preserve meaning, not merely remove parser errors.
//! Authored Groovy fixtures are parsed only, with explicit source-span assertions.

use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

fn parse(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
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

fn point(source: &str, offset: usize) -> Point {
    let prefix = &source[..offset];
    Point::new(
        prefix.bytes().filter(|b| *b == b'\n').count(),
        prefix
            .rfind('\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn signature(tree: &Tree) -> Vec<(String, std::ops::Range<usize>, bool)> {
    let mut rows: Vec<_> = nodes(tree.root_node())
        .iter()
        .map(|n| (n.kind().into(), n.byte_range(), n.is_missing()))
        .collect();
    rows.sort_by(|a, b| (&a.0, a.1.start, a.1.end, a.2).cmp(&(&b.0, b.1.start, b.1.end, b.2)));
    rows
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

fn exact(tree: &Tree, source: &str, kind: &str, text: &str) {
    assert!(
        nodes(tree.root_node())
            .iter()
            .any(|node| node.kind() == kind && node.utf8_text(source.as_bytes()).unwrap() == text),
        "missing {kind} {text:?}: {}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn semicolons_separate_script_and_closure_statements() {
    let source = "def first = 1; def second = 2;\ndef run() { def a = 1; def b = 2; a + b; }\n";
    let tree = valid(source);
    exact(&tree, source, "local_variable_declaration", "def first = 1");
    exact(
        &tree,
        source,
        "local_variable_declaration",
        "def second = 2",
    );
}

#[test]
fn semicolons_separate_fields_and_abstract_methods() {
    valid(
        "abstract class Meter { int value = 1; abstract int measure(); int read() { value; } }\n",
    );
}

#[test]
fn no_separator_between_declarations_is_rejected() {
    for source in [
        "def first = 1 def second = 2",
        "def run() { def first = 1 def second = 2 }",
    ] {
        assert!(
            parse(source).root_node().has_error(),
            "silently joined {source}"
        );
    }
}

#[test]
fn return_does_not_capture_next_line_call() {
    let source = "def run() { return\nfinish() }\n";
    let tree = valid(source);
    exact(&tree, source, "return_statement", "return");
    exact(&tree, source, "method_invocation", "finish()");
}

#[test]
fn leading_operator_starts_a_new_statement() {
    let source = "def run() { first\n+ second }\n";
    let tree = valid(source);
    exact(&tree, source, "expression_statement", "first");
    exact(&tree, source, "unary_expression", "+ second");
}

#[test]
fn trailing_operator_and_parentheses_allow_continuation() {
    for source in [
        "def value = first +\nsecond\n",
        "consume(\nfirst +\nsecond,\nthird\n)\n",
        "def values = [\nfirst,\nsecond\n]\n",
    ] {
        valid(source);
    }
}

#[test]
fn method_bodies_may_start_on_the_next_line() {
    valid("class Meter {\nint read()\n{\nreturn 1\n}\nint twice()\n{\nread() * 2\n}\n}\n");
}

#[test]
fn conditional_and_loop_bodies_keep_semicolons() {
    valid(
        "if (ready) first(); else second();\nwhile (ready) advance();\ndo { advance() } while (ready);\nfor (i = 0; i < 3; i++) advance();\n",
    );
}

#[test]
fn newline_comments_do_not_hide_statement_boundaries() {
    let source = "def first = 1 // note\ndef second = 2\n";
    let tree = valid(source);
    exact(&tree, source, "local_variable_declaration", "def first = 1");
    exact(
        &tree,
        source,
        "local_variable_declaration",
        "def second = 2",
    );
}

#[test]
fn crlf_and_blank_lines_preserve_exact_statements() {
    let source = "\r\ndef first = 'λ😀'\r\n\r\ndef second = 2;\r\n";
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
}

#[test]
fn conditional_alternatives_allow_newlines() {
    for source in [
        "if (ready) { first() }\nelse { second() }\n",
        "if (ready) first();\nelse second();\n",
    ] {
        let tree = valid(source);
        exact(
            &tree,
            source,
            "if_statement",
            source.trim_end().trim_end_matches(';'),
        );
    }
}

#[test]
fn do_while_condition_allows_newlines() {
    valid("do { advance() }\nwhile (ready)\n");
}

#[test]
fn catch_and_finally_allow_newlines() {
    valid("try { load() }\ncatch (Exception error) { recover(error) }\nfinally { close() }\n");
}

#[test]
fn method_throws_and_body_allow_newlines() {
    valid("class Meter {\nint read()\nthrows Exception\n{\nreturn 1\n}\n}\n");
}

#[test]
fn closure_parameters_allow_newlines() {
    valid("def run = {\nint value ->\nvalue + 1\n}\n");
}

#[test]
fn leading_member_access_continues_expression() {
    let source = "def result = value\n.toString()\n";
    let tree = valid(source);
    exact(&tree, source, "method_invocation", "value\n.toString()");
}

#[test]
fn declaration_initializer_allows_newline_before_assignment() {
    let source = "def value\n= 1\n";
    let tree = valid(source);
    exact(
        &tree,
        source,
        "local_variable_declaration",
        "def value\n= 1",
    );
}

#[test]
fn do_while_rejects_nonblock_or_separated_bodies() {
    for source in [
        "do advance(); while (ready);",
        "do { advance() }; while (ready);",
    ] {
        assert!(
            parse(source).root_node().has_error(),
            "accepted invalid do-while: {source}"
        );
    }
}

#[test]
fn incremental_statement_edits_match_fresh_trees() {
    let source = "def run() { def first = 'λ😀'; return\nfinish() }\n";
    for (before, after) in [
        (";", "\r\n"),
        ("return\n", "return "),
        ("λ😀", "δ🌍!"),
        ("finish()", "finish(\nfirst\n)"),
    ] {
        let start = source.find(before).unwrap();
        let mut changed = source.to_owned();
        changed.replace_range(start..start + before.len(), after);
        let mut parser = Parser::new();
        parser
            .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
            .unwrap();
        let mut old = parser.parse(source, None).unwrap();
        old.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + before.len(),
            new_end_byte: start + after.len(),
            start_position: point(source, start),
            old_end_position: point(source, start + before.len()),
            new_end_position: point(&changed, start + after.len()),
        });
        assert_eq!(
            signature(&parser.parse(&changed, Some(&old)).unwrap()),
            signature(&valid(&changed))
        );
    }
}

#[test]
fn included_statement_ranges_preserve_host_coordinates() {
    let prefix = "# λ😀\r\n```groovy\r\n";
    let body = "def first = 1;\r\ndef second = 2\r\n";
    let source = format!("{prefix}{body}```\r\n");
    let start = prefix.len();
    let end = start + body.len();
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    parser
        .set_included_ranges(&[tree_sitter::Range {
            start_byte: start,
            end_byte: end,
            start_point: point(&source, start),
            end_point: point(&source, end),
        }])
        .unwrap();
    let tree = parser.parse(&source, None).unwrap();
    assert!(!tree.root_node().has_error());
    for node in nodes(tree.root_node()) {
        assert!(node.start_byte() >= start && node.end_byte() <= end);
        assert!(source.get(node.byte_range()).is_some());
        assert_eq!(node.start_position(), point(&source, node.start_byte()));
        assert_eq!(node.end_position(), point(&source, node.end_byte()));
    }
    exact(
        &tree,
        &source,
        "local_variable_declaration",
        "def second = 2",
    );
}

#[test]
fn malformed_statement_reset_cannot_retain_scanner_context() {
    let source = "def run() { return\nfinish() }\n";
    let expected = signature(&valid(source));
    let mut parser = Parser::new();
    parser
        .set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    for damaged in [
        "def run() { return\n",
        "def x = value\n.",
        "def run() { def a = 1 def b = 2 }",
    ] {
        assert!(
            parser.parse(damaged, None).unwrap().root_node().has_error(),
            "{damaged}"
        );
        parser.reset();
        assert_eq!(signature(&parser.parse(source, None).unwrap()), expected);
    }
}
