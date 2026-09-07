//! Checks ordered heredoc groups without evaluating shell commands.
//! Native source spans and nested lifetimes must survive incremental parsing.

use tree_sitter::{InputEdit, Node, Parser, Point};

const NESTED_BODY: &str =
    "cat <<OUTER <<TAIL\n$(cat <<INNER\ninside\nINNER\n)\nOUTER\nlast\nTAIL\nafter() { :; }\n";
const NESTED_HEADER: &str = "cat <<OUTER \"$(cat <<INNER\ninside\nINNER\n)\" <<TAIL\nouter\nOUTER\nlast\nTAIL\nafter() { :; }\n";

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
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
fn same_command_heredocs_follow_declaration_order() {
    for (source, ends) in [
        (
            "cat <<ONE <<TWO\nfirst\nONE\nsecond\nTWO\nafter() { :; }\n",
            vec!["ONE", "TWO"],
        ),
        (
            "cat <<ONE <<'TWO' <<終端\nfirst $value\nONE\nsecond $literal\nTWO\nthird\n終端\nafter() { :; }\n",
            vec!["ONE", "TWO", "終端"],
        ),
    ] {
        let tree = parser().parse(source, None).expect("parse completes");
        let root = tree.root_node();
        assert!(!root.has_error(), "{source:?}: {}", root.to_sexp());
        let actual: Vec<_> = nodes(root, "heredoc_end")
            .into_iter()
            .map(|node| node.utf8_text(source.as_bytes()).expect("source bytes"))
            .collect();
        assert_eq!(actual, ends);
        let functions = nodes(root, "function_definition");
        assert_eq!(functions.len(), 1);
        assert_eq!(
            functions[0]
                .utf8_text(source.as_bytes())
                .expect("function bytes"),
            "after() { :; }"
        );
    }
}

fn assert_group(source: &str, expected_ends: &[&str]) {
    let tree = parser().parse(source, None).expect("parse completes");
    let root = tree.root_node();
    assert!(!root.has_error(), "{source:?}: {}", root.to_sexp());
    let ends: Vec<_> = nodes(root, "heredoc_end")
        .into_iter()
        .map(|node| node.utf8_text(source.as_bytes()).expect("end source"))
        .collect();
    assert_eq!(ends, expected_ends);
    let functions = nodes(root, "function_definition");
    assert_eq!(functions.len(), 1, "{}", root.to_sexp());
    assert_eq!(
        functions[0]
            .utf8_text(source.as_bytes())
            .expect("function source"),
        "after() { :; }"
    );
}

#[test]
fn nested_body_substitution_keeps_its_own_group() {
    assert_group(NESTED_BODY, &["INNER", "OUTER", "TAIL"]);
}

#[test]
fn nested_header_substitution_keeps_pending_outer_delimiters() {
    assert_group(NESTED_HEADER, &["INNER", "OUTER", "TAIL"]);
}

#[test]
fn descriptors_and_arguments_can_separate_delimiters() {
    assert_group(
        "cat 3<<ONE argument >output 4<<-TWO tail\nfirst\nONE\n\tsecond\n\tTWO\nafter() { :; }\n",
        &["ONE", "TWO"],
    );
}

#[test]
fn consecutive_groups_do_not_merge_lifetimes() {
    assert_group(
        "cat <<ONE <<TWO\none\nONE\ntwo\nTWO\ncat <<THREE <<FOUR\nthree\nTHREE\nfour\nFOUR\nafter() { :; }\n",
        &["ONE", "TWO", "THREE", "FOUR"],
    );
}

#[test]
fn body_ranges_and_quote_modes_remain_independent() {
    let source = "cat <<ONE <<'TWO'\nfirst $value\nONE\nsecond $literal\nTWO\nafter() { :; }\n";
    let tree = parser().parse(source, None).expect("parse");
    assert!(!tree.root_node().has_error());
    let bodies: Vec<_> = nodes(tree.root_node(), "heredoc_body")
        .into_iter()
        .map(|node| node.utf8_text(source.as_bytes()).expect("body source"))
        .collect();
    assert_eq!(bodies, ["first $value\n", "second $literal\n"]);
    let expansions: Vec<_> = nodes(tree.root_node(), "simple_expansion")
        .into_iter()
        .map(|node| node.utf8_text(source.as_bytes()).expect("expansion source"))
        .collect();
    assert_eq!(expansions, ["$value"]);
}

#[test]
fn incomplete_or_wrongly_ordered_groups_report_parse_errors() {
    for source in [
        "cat <<ONE <<TWO\nfirst\nONE\nsecond\n",
        "cat <<ONE <<TWO\nsecond\nTWO\nfirst\nONE\n",
        "cat <<ONE <<TWO\nfirst\nONE\nsecond\nTWOsuffix\n",
        "cat <<ONE <<TWO\nONE\n",
    ] {
        let tree = parser().parse(source, None).expect("parse");
        assert!(
            tree.root_node().has_error(),
            "{source:?}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn empty_continued_and_subshell_groups_preserve_source() {
    for source in [
        "cat <<ONE <<TWO\nONE\nTWO\nafter() { :; }\n",
        "cat <<ONE \\\n<<TWO\nfirst\nONE\nsecond\nTWO\nafter() { :; }\n",
        "(cat <<ONE <<TWO\nfirst\nONE\nsecond\nTWO\n)\nafter() { :; }\n",
    ] {
        assert_group(source, &["ONE", "TWO"]);
    }
}

#[test]
fn deep_nesting_preserves_each_complete_delimiter_line() {
    assert_group(
        "cat <<A\n$(cat <<B\n$(cat <<C\ninner\nC\n)\nB\n)\nA\nafter() { :; }\n",
        &["C", "B", "A"],
    );
}

fn point(source: &str, byte: usize) -> Point {
    let prefix = &source[..byte];
    Point {
        row: prefix.bytes().filter(|byte| *byte == b'\n').count(),
        column: prefix.rsplit('\n').next().expect("line").len(),
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Span {
    kind: String,
    start: usize,
    end: usize,
    children: usize,
    named: bool,
    missing: bool,
    error: bool,
}

fn spans(root: Node<'_>) -> Vec<Span> {
    let mut result = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        result.push(Span {
            kind: node.kind().to_owned(),
            start: node.start_byte(),
            end: node.end_byte(),
            children: node.child_count(),
            named: node.is_named(),
            missing: node.is_missing(),
            error: node.is_error(),
        });
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result
}

#[test]
fn nested_group_edits_match_fresh_source_spans() {
    let mut incremental_parser = parser();
    for source in [NESTED_HEADER, NESTED_BODY] {
        let initial = incremental_parser
            .parse(source, None)
            .expect("initial parse");
        assert!(!initial.root_node().has_error());
        for (start, character) in source.char_indices() {
            let end = start + character.len_utf8();
            for replacement in ["", "x", "\n", "終端"] {
                let mut changed = source.to_owned();
                changed.replace_range(start..end, replacement);
                let mut edited = initial.clone();
                edited.edit(&InputEdit {
                    start_byte: start,
                    old_end_byte: end,
                    new_end_byte: start + replacement.len(),
                    start_position: point(source, start),
                    old_end_position: point(source, end),
                    new_end_position: point(&changed, start + replacement.len()),
                });
                let incremental = incremental_parser
                    .parse(&changed, Some(&edited))
                    .expect("incremental parse");
                let fresh = parser().parse(&changed, None).expect("fresh parse");
                assert_eq!(
                    spans(incremental.root_node()),
                    spans(fresh.root_node()),
                    "edit {start} -> {replacement:?}: {source:?}"
                );
            }
        }
        incremental_parser.reset();
        let reset = incremental_parser.parse(source, None).expect("reset parse");
        assert_eq!(spans(reset.root_node()), spans(initial.root_node()));
    }
}
