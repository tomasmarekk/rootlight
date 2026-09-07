//! Checks ordered heredoc groups without evaluating shell commands.
//! Native source spans and nested lifetimes must survive incremental parsing.

use tree_sitter::{InputEdit, Node, Parser, Point};

const NESTED_BODY: &str =
    "cat <<OUTER <<TAIL\n$(cat <<INNER\ninside\nINNER\n)\nOUTER\nlast\nTAIL\nafter() { :; }\n";
const NESTED_HEADER: &str = "cat <<OUTER \"$(cat <<INNER\ninside\nINNER\n)\" <<TAIL\nouter\nOUTER\nlast\nTAIL\nafter() { :; }\n";
const CONNECTED_NESTED: &str = "left <<ONE | right <<TWO\n$(inner <<THREE | last <<FOUR\nthird\nTHREE\nfourth\nFOUR\n)\nONE\nsecond\nTWO\nafter() { :; }\n";

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
fn connected_commands_share_declaration_order() {
    for operator in ["|", "|&", "&&", "||"] {
        let source = format!(
            "left <<ONE {operator} right <<TWO\nfirst\nONE\nsecond\nTWO\nafter() {{ :; }}\n"
        );
        assert_group(&source, &["ONE", "TWO"]);
        let tree = parser().parse(&source, None).expect("parse");
        let names: Vec<_> = nodes(tree.root_node(), "command_name")
            .into_iter()
            .map(|node| node.utf8_text(source.as_bytes()).expect("command source"))
            .collect();
        assert_eq!(names, ["left", "right", ":"]);
    }
}

#[test]
fn connected_arguments_remain_owned_by_the_command() {
    for operator in ["|", "|&", "&&", "||"] {
        let source = format!("left <<ONE {operator} right argument\nbody\nONE\n");
        let tree = parser().parse(&source, None).expect("parse");
        let root = tree.root_node();
        assert!(!root.has_error(), "{}", root.to_sexp());
        assert_eq!(nodes(root, "redirected_statement").len(), 1);
        let commands = nodes(root, "command");
        let right = commands
            .iter()
            .find(|command| {
                command.child_by_field_name("name").is_some_and(|name| {
                    name.utf8_text(source.as_bytes()).expect("name source") == "right"
                })
            })
            .expect("right command");
        let argument = right.child_by_field_name("argument").expect("argument");
        assert_eq!(
            argument
                .utf8_text(source.as_bytes())
                .expect("argument source"),
            "argument"
        );
    }
}

#[test]
fn mixed_connections_preserve_each_body_and_command() {
    let source = "left <<ONE <<TWO | MODE=value middle 3<<'THREE' tail >output && last <<-FOUR\nfirst\nONE\nsecond\nTWO\nthird $literal\nTHREE\n\tfourth\n\tFOUR\nafter() { :; }\n";
    assert_group(source, &["ONE", "TWO", "THREE", "FOUR"]);
    let tree = parser().parse(source, None).expect("parse");
    let bodies: Vec<_> = nodes(tree.root_node(), "heredoc_body")
        .into_iter()
        .map(|node| node.utf8_text(source.as_bytes()).expect("body source"))
        .collect();
    // These are physical source spans, not the shell's tab-stripped input values.
    assert_eq!(
        bodies,
        ["first\n", "second\n", "third $literal\n", "fourth\n\t"]
    );
    let names: Vec<_> = nodes(tree.root_node(), "command_name")
        .into_iter()
        .map(|node| node.utf8_text(source.as_bytes()).expect("command source"))
        .collect();
    assert_eq!(names, ["left", "middle", "last", ":"]);
    assert!(nodes(tree.root_node(), "simple_expansion").is_empty());
}

#[test]
fn connected_headers_and_bodies_isolate_nested_substitutions() {
    assert_group(CONNECTED_NESTED, &["THREE", "FOUR", "ONE", "TWO"]);
    assert_group(
        "left <<ONE || right \"$(inner <<THREE\nthird\nTHREE\n)\" <<TWO tail\nfirst\nONE\nsecond\nTWO\nafter() { :; }\n",
        &["THREE", "ONE", "TWO"],
    );
}

#[test]
fn connected_commands_keep_complete_syntax_without_new_inputs() {
    for (source, ends) in [
        (
            "first | left <<ONE | ! right <<TWO\nfirst\nONE\nsecond\nTWO\nafter() { :; }\n",
            vec!["ONE", "TWO"],
        ),
        (
            "left <<ONE | middle arg | last >output\nfirst\nONE\nafter() { :; }\n",
            vec!["ONE"],
        ),
        (
            "left <<ONE || right <<TWO | last <<THREE\nfirst\nONE\nsecond\nTWO\nthird\nTHREE\nafter() { :; }\n",
            vec!["ONE", "TWO", "THREE"],
        ),
        (
            "left <<ONE && (right) <<TWO\nfirst\nONE\nsecond\nTWO\nafter() { :; }\n",
            vec!["ONE", "TWO"],
        ),
    ] {
        assert_group(source, &ends);
    }
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
        "left <<ONE | right <<TWO\nfirst\nONE\nsecond\n",
        "left <<ONE && right <<TWO\nsecond\nTWO\nfirst\nONE\n",
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
    for source in [NESTED_HEADER, NESTED_BODY, CONNECTED_NESTED] {
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
