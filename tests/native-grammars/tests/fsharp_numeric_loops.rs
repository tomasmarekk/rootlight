//! Numeric wildcard loops retain their direct binder and expression ancestry.
//! These implementation-only controls distinguish syntax ownership from type
//! checking; signature bodies and arbitrary numeric-loop patterns are excluded.

use std::{
    ops::{ControlFlow, Range},
    time::{Duration, Instant},
};
use tree_sitter::{
    InputEdit, Node, ParseOptions, Parser, Point, Query, QueryCursor, StreamingIterator, Tree,
};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_fsharp::LANGUAGE_FSHARP.into())
        .expect("pinned implementation grammar is incompatible with Tree-sitter");
    parser
}

fn parse(parser: &mut Parser, source: &str, old: Option<&Tree>) -> Tree {
    assert!(source.len() <= 4_096);
    let started = Instant::now();
    let mut checks = 0;
    let mut watchdog = |_: &tree_sitter::ParseState| {
        checks += 1;
        if checks > 2_000 || started.elapsed() > Duration::from_secs(10) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    parser
        .parse_with_options(
            &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
            old,
            Some(ParseOptions::new().progress_callback(&mut watchdog)),
        )
        .expect("authored loop exceeded its parse watchdog")
}

fn point(source: &str, offset: usize) -> Point {
    let prefix = source.as_bytes().get(..offset).unwrap();
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn nodes(tree: &Tree) -> Vec<Node<'_>> {
    let mut pending = vec![tree.root_node()];
    let mut found = Vec::new();
    while let Some(node) = pending.pop() {
        assert!(found.len() < 10_000, "authored loop exceeds its node bound");
        found.push(node);
        for index in (0..node.child_count()).rev() {
            pending.push(node.child(u32::try_from(index).unwrap()).unwrap());
        }
    }
    found
}

fn text<'source>(node: Node<'_>, source: &'source str) -> &'source str {
    source.get(node.byte_range()).unwrap()
}

fn span(node: Node<'_>, source: &str, written: &str) {
    assert_eq!(text(node, source), written, "{}", node.to_sexp());
    assert_eq!(node.start_position(), point(source, node.start_byte()));
    assert_eq!(node.end_position(), point(source, node.end_byte()));
    assert_eq!(node.end_byte() - node.start_byte(), written.len());
}

fn exact<'tree>(tree: &'tree Tree, source: &str, kind: &str, written: &str) -> Node<'tree> {
    let matches: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == kind && text(*node, source) == written)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "{kind} {written:?}: {}",
        tree.root_node().to_sexp()
    );
    let node = matches[0];
    span(node, source, written);
    node
}

fn direct<'tree>(owner: Node<'tree>, source: &str, kind: &str, written: &str) -> Node<'tree> {
    let matches: Vec<_> = (0..owner.child_count())
        .map(|index| owner.child(u32::try_from(index).unwrap()).unwrap())
        .filter(|node| node.kind() == kind && text(*node, source) == written)
        .collect();
    assert_eq!(matches.len(), 1, "{}: {kind} {written:?}", owner.to_sexp());
    let node = matches[0];
    span(node, source, written);
    assert_eq!(node.parent(), Some(owner));
    node
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    kind: String,
    bytes: Range<usize>,
    start: Point,
    end: Point,
    parent: Option<String>,
    fields: Vec<Option<String>>,
    named: bool,
    extra: bool,
    error: bool,
    missing: bool,
    contains_error: bool,
}

fn snapshot(tree: &Tree, source: &str) -> (String, Vec<Snapshot>) {
    let rows = nodes(tree)
        .into_iter()
        .map(|node| {
            assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
            assert_eq!(node.start_position(), point(source, node.start_byte()));
            assert_eq!(node.end_position(), point(source, node.end_byte()));
            Snapshot {
                kind: node.kind().to_owned(),
                bytes: node.byte_range(),
                start: node.start_position(),
                end: node.end_position(),
                parent: node.parent().map(|parent| parent.kind().to_owned()),
                fields: (0..node.child_count())
                    .map(|index| {
                        node.field_name_for_child(u32::try_from(index).unwrap())
                            .map(str::to_owned)
                    })
                    .collect(),
                named: node.is_named(),
                extra: node.is_extra(),
                error: node.is_error(),
                missing: node.is_missing(),
                contains_error: node.has_error(),
            }
        })
        .collect();
    (tree.root_node().to_sexp(), rows)
}

fn variants(loop_source: &str, mut check: impl FnMut(&str, &str)) {
    let source = format!(
        "module LoopExample\nlet iterate values =\n    {loop_source}\nlet following = 30\n"
    );
    for newline in ["\n", "\r\n"] {
        for final_newline in [false, true] {
            let base = if final_newline {
                source.as_str()
            } else {
                source.strip_suffix('\n').unwrap()
            };
            check(
                &base.replace('\n', newline),
                &loop_source.replace('\n', newline),
            );
        }
    }
}

fn queries(tree: &Tree, source: &str, loop_node: Node<'_>) {
    let query = Query::new(
        &tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
        "(_expression) @expression\n(for_expression) @loop",
    )
    .expect("expression and explicit loop queries must compile");
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(128);
    let mut expression_loops = Vec::new();
    let mut explicit_loops = Vec::new();
    let mut capture_count = 0;
    {
        let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
        while let Some((found, index)) = captures.next() {
            capture_count += 1;
            assert!(capture_count < 1_024);
            let capture = found.captures[*index];
            if capture.node.kind() == "for_expression" {
                span(capture.node, source, text(loop_node, source));
                match query.capture_names()[usize::try_from(capture.index).unwrap()] {
                    "expression" => expression_loops.push(capture.node),
                    "loop" => explicit_loops.push(capture.node),
                    other => panic!("unexpected capture {other}"),
                }
            }
        }
    }
    assert!(!cursor.did_exceed_match_limit());
    assert_eq!(
        expression_loops,
        [loop_node],
        "loop lost expression ancestry"
    );
    assert_eq!(explicit_loops, [loop_node]);
}

fn clean<'tree>(
    tree: &'tree Tree,
    source: &str,
    written_loop: &str,
    binder_kind: &str,
    binder: &str,
) -> Node<'tree> {
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().kind(), "file");
    assert_eq!(tree.root_node().end_byte(), source.len());
    snapshot(tree, source);
    let loop_node = exact(tree, source, "for_expression", written_loop);
    assert_eq!(
        nodes(tree)
            .iter()
            .filter(|node| node.kind() == "for_expression")
            .count(),
        1
    );
    let binder_node = exact(tree, source, binder_kind, binder);
    assert_eq!(binder_node.parent(), Some(loop_node));
    let children: Vec<_> = (0..loop_node.named_child_count())
        .map(|index| {
            loop_node
                .named_child(u32::try_from(index).unwrap())
                .unwrap()
        })
        .filter(|node| !node.is_extra())
        .collect();
    assert_eq!(children.first(), Some(&binder_node));
    let body = exact(tree, source, "application_expression", "ignore 0");
    assert_eq!(body.parent(), Some(loop_node));
    assert_eq!(children.last(), Some(&body));
    let binding = loop_node.parent().unwrap();
    assert_eq!(binding.kind(), "function_or_value_defn");
    assert_eq!(binding.child_by_field_name("body"), Some(loop_node));
    let declaration = binding.parent().unwrap();
    assert_eq!(declaration.kind(), "declaration_expression");
    let module = declaration.parent().unwrap();
    assert_eq!(module.kind(), "named_module");
    let following = exact(tree, source, "function_or_value_defn", "let following = 30");
    let following_declaration = following.parent().unwrap();
    assert_eq!(following_declaration.kind(), "declaration_expression");
    assert_eq!(following_declaration.parent(), Some(module));
    assert_ne!(declaration, following_declaration);
    let following_body = following.child_by_field_name("body").unwrap();
    assert_eq!(following_body.kind(), "const");
    span(following_body, source, "30");
    assert!(loop_node.end_byte() < following.start_byte());
    assert!(declaration.end_byte() <= following_declaration.start_byte());
    queries(tree, source, loop_node);
    loop_node
}

fn numeric(
    tree: &Tree,
    source: &str,
    loop_node: Node<'_>,
    lower: &str,
    direction: &str,
    upper: &str,
) {
    assert_eq!(
        (0..loop_node.named_child_count())
            .map(|index| loop_node
                .named_child(u32::try_from(index).unwrap())
                .unwrap())
            .filter(|node| !node.is_extra())
            .count(),
        4
    );
    for written in [lower, upper] {
        let children: Vec<_> = (0..loop_node.named_child_count())
            .map(|index| {
                loop_node
                    .named_child(u32::try_from(index).unwrap())
                    .unwrap()
            })
            .filter(|node| !node.is_extra() && text(*node, source) == written)
            .collect();
        assert_eq!(children.len(), 1);
        span(children[0], source, written);
        assert_eq!(children[0].parent(), Some(loop_node));
    }
    for written in ["=", direction] {
        let token = direct(loop_node, source, written, written);
        assert_eq!(token.parent(), Some(loop_node));
    }
    assert!(!nodes(tree).iter().any(|node| node.kind() == "in"));
}

#[test]
fn numeric_wildcard_to_and_downto_keep_direct_binder_and_bounds() {
    for (loop_source, lower, direction, upper) in [
        ("for _ = 1 to 3 do\n        ignore 0", "1", "to", "3"),
        (
            "for _ = 3 downto 1 do\n        ignore 0",
            "3",
            "downto",
            "1",
        ),
    ] {
        variants(loop_source, |source, written| {
            let tree = parse(&mut parser(), source, None);
            let node = clean(&tree, source, written, "wildcard_pattern", "_");
            numeric(&tree, source, node, lower, direction, upper);
        });
    }
}

#[test]
fn identifier_numeric_loops_keep_existing_binder_and_named_bounds() {
    for direction in ["to", "downto"] {
        variants(
            &format!("for index = start {direction} finish do ignore 0"),
            |source, written| {
                let tree = parse(&mut parser(), source, None);
                let node = clean(&tree, source, written, "identifier", "index");
                numeric(&tree, source, node, "start", direction, "finish");
                assert!(
                    !nodes(&tree)
                        .iter()
                        .any(|node| node.kind() == "wildcard_pattern")
                );
            },
        );
    }
}

#[test]
fn for_in_retains_wildcard_tuple_and_record_pattern_routes() {
    for (kind, pattern) in [
        ("wildcard_pattern", "_"),
        ("paren_pattern", "(left, right)"),
        ("record_pattern", "{ Value = value }"),
    ] {
        variants(
            &format!("for {pattern} in values do ignore 0"),
            |source, written| {
                let tree = parse(&mut parser(), source, None);
                let node = clean(&tree, source, written, kind, pattern);
                let keyword = exact(&tree, source, "in", "in");
                assert_eq!(keyword.parent(), Some(node));
                assert_eq!(
                    (0..node.named_child_count())
                        .map(|index| node.named_child(u32::try_from(index).unwrap()).unwrap())
                        .filter(|child| !child.is_extra())
                        .count(),
                    3
                );
                let values = exact(&tree, source, "long_identifier_or_op", "values");
                assert_eq!(values.parent(), Some(node));
                if kind == "paren_pattern" {
                    let tuple = exact(&tree, source, "repeat_pattern", "left, right");
                    assert_eq!(tuple.parent().unwrap().parent(), Some(node));
                }
                if kind == "record_pattern" {
                    let field = exact(&tree, source, "field_pattern", "Value = value");
                    assert_eq!(field.parent().unwrap().parent(), Some(node));
                }
            },
        );
    }
}

#[test]
fn nested_block_comment_between_wildcard_and_equals_stays_trivia() {
    let comment = "(* outer (* nested *) tail *)";
    variants(
        &format!("for _ {comment} = 1 to 3 do\n        ignore 0"),
        |source, written| {
            let tree = parse(&mut parser(), source, None);
            let node = clean(&tree, source, written, "wildcard_pattern", "_");
            numeric(&tree, source, node, "1", "to", "3");
            let trivia = exact(&tree, source, "block_comment", comment);
            assert!(trivia.is_extra());
            assert!(trivia.start_byte() > exact(&tree, source, "wildcard_pattern", "_").end_byte());
            assert!(trivia.end_byte() < direct(node, source, "=", "=").start_byte());
        },
    );
}

#[test]
fn line_comment_after_do_does_not_own_scoped_body_or_following_binding() {
    variants(
        "for _ = 1 to 3 do // body\n        ignore 0",
        |source, written| {
            let tree = parse(&mut parser(), source, None);
            let node = clean(&tree, source, written, "wildcard_pattern", "_");
            numeric(&tree, source, node, "1", "to", "3");
            let comment = exact(&tree, source, "line_comment", "// body");
            let body = exact(&tree, source, "application_expression", "ignore 0");
            assert!(comment.is_extra());
            assert!(comment.end_byte() < body.start_byte());
        },
    );
}

#[test]
fn inline_numeric_body_and_done_keep_both_expression_query_routes() {
    variants("for _ = 3 downto 1 do ignore 0 done", |source, written| {
        let tree = parse(&mut parser(), source, None);
        let node = clean(&tree, source, written, "wildcard_pattern", "_");
        numeric(&tree, source, node, "3", "downto", "1");
        let done = exact(&tree, source, "done", "done");
        assert_eq!(done.parent(), Some(node));
        assert!(
            exact(&tree, source, "application_expression", "ignore 0").end_byte()
                < done.start_byte()
        );
    });
}

fn edit(tree: &mut Tree, before: &str, after: &str, start: usize, old_end: usize, new_end: usize) {
    tree.edit(&InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point(before, start),
        old_end_position: point(before, old_end),
        new_end_position: point(after, new_end),
    });
    assert!(tree.root_node().has_changes());
    assert_eq!(tree.root_node().end_byte(), after.len());
    assert_eq!(tree.root_node().end_position(), point(after, after.len()));
}

#[test]
fn length_changing_wildcard_identifier_edit_and_restore_matches_fresh_tree() {
    for direction in ["to", "downto"] {
        variants(
            &format!("for _ = 1 {direction} 3 do\n        ignore 0"),
            |source, written| {
                let mut reused = parser();
                let original = parse(&mut reused, source, None);
                clean(&original, source, written, "wildcard_pattern", "_");
                let binder = exact(&original, source, "wildcard_pattern", "_");
                let start = binder.start_byte();
                let old_end = binder.end_byte();
                let mut changed = source.to_owned();
                changed.replace_range(start..old_end, "loopIndex");
                let changed_loop = written.replacen("for _", "for loopIndex", 1);
                let mut edited = original.clone();
                edit(
                    &mut edited,
                    source,
                    &changed,
                    start,
                    old_end,
                    start + "loopIndex".len(),
                );
                let mut incremental = parse(&mut reused, &changed, Some(&edited));
                let fresh = parse(&mut parser(), &changed, None);
                clean(
                    &incremental,
                    &changed,
                    &changed_loop,
                    "identifier",
                    "loopIndex",
                );
                assert_eq!(snapshot(&incremental, &changed), snapshot(&fresh, &changed));
                edit(
                    &mut incremental,
                    &changed,
                    source,
                    start,
                    start + "loopIndex".len(),
                    old_end,
                );
                let restored = parse(&mut reused, source, Some(&incremental));
                let fresh_restored = parse(&mut parser(), source, None);
                clean(&restored, source, written, "wildcard_pattern", "_");
                assert_eq!(
                    snapshot(&restored, source),
                    snapshot(&fresh_restored, source)
                );
                assert_eq!(snapshot(&restored, source), snapshot(&original, source));
            },
        );
    }
}

#[test]
fn numeric_tuple_and_record_binders_remain_visible_recovery_not_general_patterns() {
    for binder in ["(left, right)", "{ Value = value }"] {
        variants(
            &format!("for {binder} = 1 to 3 do ignore 0"),
            |source, _| {
                let tree = parse(&mut parser(), source, None);
                snapshot(&tree, source);
                assert!(
                    tree.root_node().has_error(),
                    "numeric pattern parsed clean: {source:?}"
                );
                // A hidden missing token may be absent from public children but is serialized.
                let expression = tree.root_node().to_sexp();
                assert!(
                    expression.contains("(ERROR") || expression.contains("(MISSING "),
                    "{expression}"
                );
            },
        );
    }
}

#[test]
fn done_terminates_named_numeric_and_while_loops_without_becoming_an_argument() {
    variants("for index = 1 to 3 do ignore 0 done", |source, written| {
        let tree = parse(&mut parser(), source, None);
        let node = clean(&tree, source, written, "identifier", "index");
        numeric(&tree, source, node, "1", "to", "3");
        let done = direct(node, source, "done", "done");
        assert!(
            exact(&tree, source, "application_expression", "ignore 0").end_byte()
                < done.start_byte()
        );
    });
    variants("while ready do ignore 0 done", |source, written| {
        let tree = parse(&mut parser(), source, None);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(tree.root_node().kind(), "file");
        assert_eq!(tree.root_node().end_byte(), source.len());
        snapshot(&tree, source);
        let node = exact(&tree, source, "while_expression", written);
        let condition = direct(node, source, "long_identifier_or_op", "ready");
        let body = direct(node, source, "application_expression", "ignore 0");
        let done = direct(node, source, "done", "done");
        assert!(condition.end_byte() < body.start_byte() && body.end_byte() < done.start_byte());
        assert_eq!(
            (0..node.named_child_count())
                .map(|index| node.named_child(u32::try_from(index).unwrap()).unwrap())
                .filter(|child| !child.is_extra())
                .count(),
            2
        );
        let binding = node.parent().unwrap();
        assert_eq!(binding.kind(), "function_or_value_defn");
        assert_eq!(binding.child_by_field_name("body"), Some(node));
        let declaration = binding.parent().unwrap();
        assert_eq!(declaration.kind(), "declaration_expression");
        let module = declaration.parent().unwrap();
        assert_eq!(module.kind(), "named_module");
        let following = exact(
            &tree,
            source,
            "function_or_value_defn",
            "let following = 30",
        );
        let following_declaration = following.parent().unwrap();
        assert_eq!(following_declaration.kind(), "declaration_expression");
        assert_eq!(following_declaration.parent(), Some(module));
        assert_ne!(declaration, following_declaration);
        let following_body = following.child_by_field_name("body").unwrap();
        assert_eq!(following_body.kind(), "const");
        span(following_body, source, "30");
        assert!(node.end_byte() < following.start_byte());
        assert!(declaration.end_byte() <= following_declaration.start_byte());
    });
}

#[test]
fn escaped_and_longer_done_identifiers_keep_complete_numeric_binder_tokens() {
    for binder in ["``done``", "doneValue", "undone", "done_1"] {
        variants(
            &format!("for {binder} = 1 to 3 do ignore 0 done"),
            |source, written| {
                let tree = parse(&mut parser(), source, None);
                let node = clean(&tree, source, written, "identifier", binder);
                numeric(&tree, source, node, "1", "to", "3");
                let done = direct(node, source, "done", "done");
                let identifier = direct(node, source, "identifier", binder);
                assert!(identifier.end_byte() < done.start_byte());
                assert!(
                    !nodes(&tree)
                        .iter()
                        .any(|child| child.kind() == "identifier" && text(*child, source) == "done")
                );
            },
        );
    }
}

// Signature checks cover identifier spelling only, never admission of loop bodies.
fn signature_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_fsharp::LANGUAGE_SIGNATURE.into())
        .expect("pinned signature grammar is incompatible with Tree-sitter");
    parser
}

fn signature_variants(source: &str, mut check: impl FnMut(&str)) {
    for newline in ["\n", "\r\n"] {
        for final_newline in [false, true] {
            let base = if final_newline {
                source
            } else {
                source.strip_suffix('\n').unwrap()
            };
            check(&base.replace('\n', newline));
        }
    }
}

#[test]
fn signature_done_identifier_requires_escaping_but_longer_words_are_not_keywords() {
    for name in ["``done``", "doneValue", "undone", "done_1"] {
        signature_variants(
            &format!("module KeywordExample\nval {name} : int\nval following : int\n"),
            |source| {
                let tree = parse(&mut signature_parser(), source, None);
                assert!(
                    !tree.root_node().has_error(),
                    "{}",
                    tree.root_node().to_sexp()
                );
                assert_eq!(tree.root_node().kind(), "file");
                assert_eq!(tree.root_node().end_byte(), source.len());
                snapshot(&tree, source);
                let value = exact(
                    &tree,
                    source,
                    "value_definition",
                    &format!("val {name} : int"),
                );
                let head = exact(&tree, source, "value_declaration_left", name);
                assert_eq!(head.parent(), Some(value));
                let identifier = exact(&tree, source, "identifier", name);
                assert_eq!(identifier.byte_range(), head.byte_range());
                assert!(
                    std::iter::successors(identifier.parent(), |node| node.parent())
                        .any(|node| node == head)
                );
                let module = value.parent().unwrap();
                assert_eq!(module.kind(), "named_module");
                let following = exact(&tree, source, "value_definition", "val following : int");
                assert_eq!(following.parent(), Some(module));
                assert_ne!(value, following);
                assert!(value.end_byte() < following.start_byte());
                let following_head = exact(&tree, source, "value_declaration_left", "following");
                assert_eq!(following_head.parent(), Some(following));
                assert!(!nodes(&tree).iter().any(|node| {
                    node.kind() == "for_expression" || node.kind() == "while_expression"
                }));
            },
        );
    }
    signature_variants(
        "module KeywordExample\nval done : int\nval following : int\n",
        |source| {
            let tree = parse(&mut signature_parser(), source, None);
            snapshot(&tree, source);
            assert!(
                tree.root_node().has_error(),
                "bare keyword became a signature identifier"
            );
            let expression = tree.root_node().to_sexp();
            assert!(
                expression.contains("(ERROR") || expression.contains("(MISSING "),
                "{expression}"
            );
        },
    );
}

fn done_binding(tree: &Tree, source: &str, body: Node<'_>) {
    let binding = body.parent().unwrap();
    assert_eq!(binding.kind(), "function_or_value_defn");
    assert_eq!(binding.child_by_field_name("body"), Some(body));
    let declaration = binding.parent().unwrap();
    assert_eq!(declaration.kind(), "declaration_expression");
    let module = declaration.parent().unwrap();
    assert_eq!(module.kind(), "named_module");
    let following = exact(tree, source, "function_or_value_defn", "let following = 30");
    let following_declaration = following.parent().unwrap();
    assert_eq!(following_declaration.kind(), "declaration_expression");
    assert_eq!(following_declaration.parent(), Some(module));
    assert_ne!(declaration, following_declaration);
    span(following.child_by_field_name("body").unwrap(), source, "30");
    assert!(body.end_byte() < following.start_byte());
    assert!(declaration.end_byte() <= following_declaration.start_byte());
}

#[test]
fn nested_and_for_in_done_tokens_close_the_nearest_owned_body() {
    for inner in ["for index = 1 to 2 do ignore 0 done", "do ignore 0 done"] {
        let outer = format!("for _ = 1 to 2 do\n        {inner}\n    done");
        variants(&outer, |source, written| {
            let tree = parse(&mut parser(), source, None);
            assert!(
                !tree.root_node().has_error(),
                "{}",
                tree.root_node().to_sexp()
            );
            snapshot(&tree, source);
            let outer = exact(&tree, source, "for_expression", written);
            let binder = direct(outer, source, "wildcard_pattern", "_");
            assert_eq!(binder.parent(), Some(outer));
            numeric(&tree, source, outer, "1", "to", "2");
            let kind = if inner.starts_with("for ") {
                "for_expression"
            } else {
                "do_expression"
            };
            let nested = exact(&tree, source, kind, inner);
            assert_eq!(nested.parent(), Some(outer));
            if kind == "for_expression" {
                direct(nested, source, "identifier", "index");
                numeric(&tree, source, nested, "1", "to", "2");
            } else {
                assert_eq!(
                    (0..nested.named_child_count())
                        .map(|index| nested.named_child(u32::try_from(index).unwrap()).unwrap())
                        .filter(|child| !child.is_extra())
                        .count(),
                    1
                );
            }
            let body = direct(nested, source, "application_expression", "ignore 0");
            let inner_done = direct(nested, source, "done", "done");
            let outer_done = direct(outer, source, "done", "done");
            assert!(body.end_byte() < inner_done.start_byte());
            assert!(inner_done.end_byte() < outer_done.start_byte());
            assert_eq!(
                nodes(&tree)
                    .iter()
                    .filter(|node| node.kind() == "done")
                    .count(),
                2
            );
            done_binding(&tree, source, outer);
        });
    }
    variants("for _ in values do ignore 0 done", |source, written| {
        let tree = parse(&mut parser(), source, None);
        let node = clean(&tree, source, written, "wildcard_pattern", "_");
        direct(node, source, "in", "in");
        direct(node, source, "long_identifier_or_op", "values");
        let body = direct(node, source, "application_expression", "ignore 0");
        let done = direct(node, source, "done", "done");
        assert!(body.end_byte() < done.start_byte());
        assert_eq!(
            (0..node.named_child_count())
                .map(|index| node.named_child(u32::try_from(index).unwrap()).unwrap())
                .filter(|child| !child.is_extra())
                .count(),
            3
        );
    });
}

#[test]
fn stray_done_cannot_become_an_ordinary_value_or_application_argument() {
    for body in ["done", "ignore 0 done"] {
        variants(body, |source, _| {
            let tree = parse(&mut parser(), source, None);
            snapshot(&tree, source);
            assert!(
                tree.root_node().has_error(),
                "stray done parsed clean: {source:?}"
            );
            let expression = tree.root_node().to_sexp();
            assert!(
                expression.contains("(ERROR") || expression.contains("(MISSING "),
                "{expression}"
            );
            assert!(!nodes(&tree).iter().any(|node| {
                node.kind() == "for_expression" || node.kind() == "while_expression"
            }));
        });
    }
}

#[test]
fn unicode_done_prefix_binders_retain_full_identifier_bytes_and_points() {
    for binder in ["doneλ", "done\u{301}"] {
        variants(
            &format!("for {binder} = 1 to 3 do ignore 0 done"),
            |source, written| {
                let tree = parse(&mut parser(), source, None);
                let node = clean(&tree, source, written, "identifier", binder);
                numeric(&tree, source, node, "1", "to", "3");
                let identifier = direct(node, source, "identifier", binder);
                let done = direct(node, source, "done", "done");
                assert_eq!(
                    identifier.end_byte() - identifier.start_byte(),
                    binder.len()
                );
                assert!(identifier.end_byte() < done.start_byte());
                assert_eq!(
                    nodes(&tree)
                        .iter()
                        .filter(|child| child.kind() == "done")
                        .count(),
                    1
                );
            },
        );
    }
}

#[test]
fn signature_unicode_done_prefixes_retain_complete_value_names_and_following() {
    for name in ["doneλ", "done\u{301}"] {
        signature_variants(
            &format!("module KeywordExample\nval {name} : int\nval following : int\n"),
            |source| {
                let tree = parse(&mut signature_parser(), source, None);
                assert!(
                    !tree.root_node().has_error(),
                    "{}",
                    tree.root_node().to_sexp()
                );
                assert_eq!(tree.root_node().kind(), "file");
                assert_eq!(tree.root_node().end_byte(), source.len());
                snapshot(&tree, source);
                let value = exact(
                    &tree,
                    source,
                    "value_definition",
                    &format!("val {name} : int"),
                );
                let head = exact(&tree, source, "value_declaration_left", name);
                assert_eq!(head.parent(), Some(value));
                let identifier = exact(&tree, source, "identifier", name);
                assert_eq!(identifier.byte_range(), head.byte_range());
                assert!(
                    std::iter::successors(identifier.parent(), |node| node.parent())
                        .any(|node| node == head)
                );
                let module = value.parent().unwrap();
                assert_eq!(module.kind(), "named_module");
                let following = exact(&tree, source, "value_definition", "val following : int");
                assert_eq!(following.parent(), Some(module));
                assert_ne!(value, following);
                assert!(value.end_byte() < following.start_byte());
                let following_head = exact(&tree, source, "value_declaration_left", "following");
                assert_eq!(following_head.parent(), Some(following));
                assert!(!nodes(&tree).iter().any(|node| {
                    node.kind() == "for_expression" || node.kind() == "while_expression"
                }));
            },
        );
    }
}

#[test]
fn module_do_declarations_do_not_adopt_the_following_conditional_module() {
    let module_source =
        "module Nested =\n    open System\n    do\n        for index in 1..2 do\n            ()";
    let nested_do_source = "do\n        for index in 1..2 do\n            ()";
    let loop_source = "for index in 1..2 do\n            ()";
    for condition in ["!FEATURE", "FEATURE"] {
        let conditional = format!("#if {condition}\n{module_source}\n#endif");
        let source = format!("do ignore 0\n{conditional}\nlet following = 30\n");
        for newline in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let base = if final_newline {
                    source.as_str()
                } else {
                    source.strip_suffix('\n').unwrap()
                };
                let source = base.replace('\n', newline);
                let conditional = conditional.replace('\n', newline);
                let module_source = module_source.replace('\n', newline);
                let nested_do_source = nested_do_source.replace('\n', newline);
                let loop_source = loop_source.replace('\n', newline);
                let tree = parse(&mut parser(), &source, None);
                assert!(
                    !tree.root_node().has_error(),
                    "{}",
                    tree.root_node().to_sexp()
                );
                let file = tree.root_node();
                assert_eq!(file.kind(), "file");
                assert_eq!(file.end_byte(), source.len());
                snapshot(&tree, &source);
                let kinds: Vec<_> = (0..file.named_child_count())
                    .map(|index| file.named_child(u32::try_from(index).unwrap()).unwrap())
                    .filter(|node| !node.is_extra())
                    .map(|node| node.kind())
                    .collect();
                assert_eq!(
                    kinds,
                    [
                        "declaration_expression",
                        "preproc_if",
                        "declaration_expression"
                    ]
                );
                let first = direct(file, &source, "declaration_expression", "do ignore 0");
                let first_do = direct(first, &source, "do", "do ignore 0");
                direct(first_do, &source, "do", "do");
                let first_body = direct(first_do, &source, "application_expression", "ignore 0");
                direct(first_body, &source, "long_identifier_or_op", "ignore");
                direct(first_body, &source, "const", "0");

                let preproc = direct(file, &source, "preproc_if", &conditional);
                direct(preproc, &source, "#if", "#if");
                direct(preproc, &source, "#endif", "#endif");
                let condition_node = preproc.child_by_field_name("condition").unwrap();
                span(condition_node, &source, condition);
                assert_eq!(condition_node.parent(), Some(preproc));
                if condition.starts_with('!') {
                    assert_eq!(condition_node.kind(), "preproc_if_not_expression");
                    direct(condition_node, &source, "identifier", "FEATURE");
                } else {
                    assert_eq!(condition_node.kind(), "identifier");
                }
                assert!(preproc.child_by_field_name("alternative").is_none());
                let module = direct(preproc, &source, "module_defn", &module_source);
                direct(module, &source, "identifier", "Nested");
                let import = direct(module, &source, "import_decl", "open System");
                let import_name = direct(import, &source, "long_identifier", "System");
                direct(import_name, &source, "identifier", "System");
                let nested_declaration =
                    direct(module, &source, "declaration_expression", &nested_do_source);
                let nested_do = direct(nested_declaration, &source, "do", &nested_do_source);
                direct(nested_do, &source, "do", "do");
                let loop_node = direct(nested_do, &source, "for_expression", &loop_source);
                let binder = direct(loop_node, &source, "identifier_pattern", "index");
                let binder_name = direct(binder, &source, "long_identifier_or_op", "index");
                direct(binder_name, &source, "identifier", "index");
                direct(loop_node, &source, "in", "in");
                let range = direct(loop_node, &source, "range_expression", "1..2");
                direct(range, &source, "const", "1");
                direct(range, &source, "..", "..");
                direct(range, &source, "const", "2");
                let body = direct(loop_node, &source, "const", "()");
                direct(body, &source, "unit", "()");
                queries(&tree, &source, loop_node);

                let following_declaration = direct(
                    file,
                    &source,
                    "declaration_expression",
                    "let following = 30",
                );
                let following = direct(
                    following_declaration,
                    &source,
                    "function_or_value_defn",
                    "let following = 30",
                );
                let following_body = following.child_by_field_name("body").unwrap();
                assert_eq!(following_body.kind(), "const");
                span(following_body, &source, "30");
                assert_eq!(following_body.parent(), Some(following));
                assert!(first.end_byte() < preproc.start_byte());
                assert!(import.end_byte() < nested_declaration.start_byte());
                assert!(module.end_byte() < preproc.end_byte());
                assert!(preproc.end_byte() < following_declaration.start_byte());
                assert!(
                    !nodes(&tree)
                        .iter()
                        .any(|node| node.kind() == "do_expression")
                );
            }
        }
    }
}
