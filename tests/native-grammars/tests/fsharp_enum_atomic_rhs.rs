//! Parenthesized enum values retain canonical case, expression, and source ownership.
//! Positive oracle fixtures are parse-only; numeric values are not evaluated.
//! Recovery controls preserve delimiters without widening signature value bodies.

use std::{
    ops::{ControlFlow, Range},
    time::{Duration, Instant},
};
use tree_sitter::{
    InputEdit, Language, Node, ParseOptions, Parser, Point, Query, QueryCursor, StreamingIterator,
    Tree,
};

fn language(signature: bool) -> Language {
    if signature {
        tree_sitter_fsharp::LANGUAGE_SIGNATURE.into()
    } else {
        tree_sitter_fsharp::LANGUAGE_FSHARP.into()
    }
}

fn parser(signature: bool) -> Parser {
    let mut parser = Parser::new();
    parser.set_language(&language(signature)).unwrap();
    parser
}

fn parse(parser: &mut Parser, source: &str, old: Option<&Tree>) -> Tree {
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
        .expect("authored enum fixture exceeded its cooperative parse watchdog")
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
        assert!(
            found.len() < 10_000,
            "authored node inventory exceeds its bound"
        );
        found.push(node);
        for index in (0..node.child_count()).rev() {
            pending.push(node.child(u32::try_from(index).unwrap()).unwrap());
        }
    }
    found
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    (0..node.named_child_count())
        .map(|index| node.named_child(u32::try_from(index).unwrap()).unwrap())
        .filter(|child| !child.is_extra())
        .collect()
}

fn text<'source>(node: Node<'_>, source: &'source str) -> &'source str {
    source.get(node.byte_range()).unwrap()
}

fn written(node: Node<'_>, source: &str, kind: &str, expected: &str) {
    assert_eq!(node.kind(), kind, "{}", node.to_sexp());
    assert_eq!(text(node, source), expected, "{}", node.to_sexp());
    assert_eq!(node.start_position(), point(source, node.start_byte()));
    assert_eq!(node.end_position(), point(source, node.end_byte()));
    assert_eq!(node.end_byte() - node.start_byte(), expected.len());
}

fn exact<'tree>(tree: &'tree Tree, source: &str, kind: &str, expected: &str) -> Node<'tree> {
    let found: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == kind && text(*node, source) == expected)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{kind} {expected:?}: {}",
        tree.root_node().to_sexp()
    );
    written(found[0], source, kind, expected);
    found[0]
}

#[derive(Debug, PartialEq, Eq)]
struct Witness {
    kind: String,
    bytes: Range<usize>,
    start: Point,
    end: Point,
    parent: Option<(String, Range<usize>)>,
    fields: Vec<Option<String>>,
    named: bool,
    extra: bool,
    error: bool,
    missing: bool,
    contains_error: bool,
}

fn snapshot(tree: &Tree, source: &str) -> (String, Vec<Witness>) {
    assert_eq!(tree.root_node().kind(), "file");
    assert_eq!(tree.root_node().start_byte(), 0);
    assert_eq!(tree.root_node().end_byte(), source.len());
    let rows = nodes(tree)
        .into_iter()
        .map(|node| {
            assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
            assert!(source.get(node.byte_range()).is_some());
            assert_eq!(node.start_position(), point(source, node.start_byte()));
            assert_eq!(node.end_position(), point(source, node.end_byte()));
            if let Some(parent) = node.parent() {
                assert!(parent.start_byte() <= node.start_byte());
                assert!(node.end_byte() <= parent.end_byte());
            }
            Witness {
                kind: node.kind().to_owned(),
                bytes: node.byte_range(),
                start: node.start_position(),
                end: node.end_position(),
                parent: node
                    .parent()
                    .map(|parent| (parent.kind().to_owned(), parent.byte_range())),
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
    // The S-expression retains hidden missing terminals omitted by Node traversal.
    (tree.root_node().to_sexp(), rows)
}

fn variants(fixture: &str, mut check: impl FnMut(&str)) {
    assert!(!fixture.contains('\r'));
    let eof = fixture.strip_suffix('\n').unwrap();
    for newline in ["\n", "\r\n"] {
        for source in [eof, fixture] {
            check(&source.replace('\n', newline));
        }
    }
}

fn fixture(signature: bool, body: &str) -> String {
    let following = if signature {
        "val following: int"
    } else {
        "let following = 30"
    };
    format!("module TypeExample\ntype Flags =\n{body}\n{following}\n")
}

fn case<'tree>(tree: &'tree Tree, source: &str, name: &str, rhs: &str) -> Node<'tree> {
    let found: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == "enum_type_case")
        .filter(|node| {
            children(*node)
                .first()
                .is_some_and(|child| text(*child, source) == name)
        })
        .collect();
    assert_eq!(found.len(), 1, "{name}: {}", tree.root_node().to_sexp());
    let node = found[0];
    let direct = children(node);
    assert_eq!(direct.len(), 2, "{}", node.to_sexp());
    written(direct[0], source, "identifier", name);
    written(
        direct[1],
        source,
        if rhs.starts_with('(') {
            "paren_expression"
        } else {
            "const"
        },
        rhs,
    );
    assert_eq!(node.start_byte(), direct[0].start_byte());
    assert_eq!(node.end_byte(), direct[1].end_byte());
    let starts: Vec<_> = source
        .match_indices(&format!("{name} ="))
        .map(|(offset, _)| offset)
        .collect();
    assert_eq!(starts, vec![node.start_byte()]);
    assert_eq!(
        source
            .get(direct[0].end_byte()..direct[1].start_byte())
            .unwrap()
            .trim(),
        "="
    );
    assert_eq!(node.parent().unwrap().kind(), "enum_type_cases");
    for child in direct {
        assert_eq!(child.parent(), Some(node));
    }
    // Existing enum cases have direct children, not invented name/value fields.
    assert!(node.child_by_field_name("name").is_none());
    assert!(node.child_by_field_name("value").is_none());
    node
}

fn following<'tree>(tree: &'tree Tree, source: &str, signature: bool) -> Node<'tree> {
    let modules: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == "named_module")
        .collect();
    assert_eq!(modules.len(), 1);
    let module = modules[0];
    assert_eq!(module.kind(), "named_module");
    assert_eq!(module.parent(), Some(tree.root_node()));
    let module_name = source
        .lines()
        .find_map(|line| line.strip_prefix("module "))
        .unwrap();
    written(
        module.child_by_field_name("name").unwrap(),
        source,
        "long_identifier",
        module_name,
    );
    if signature {
        let value = exact(tree, source, "value_definition", "val following: int");
        assert_eq!(value.parent(), Some(module));
        let head = exact(tree, source, "value_declaration_left", "following");
        assert_eq!(head.parent(), Some(value));
        assert!(value.child_by_field_name("body").is_none());
        let ty = exact(tree, source, "simple_type", "int");
        assert!(std::iter::successors(ty.parent(), |node| node.parent()).any(|node| node == value));
        value
    } else {
        let wrapper = exact(tree, source, "declaration_expression", "let following = 30");
        let value = exact(tree, source, "function_or_value_defn", "let following = 30");
        assert_eq!(wrapper.parent(), Some(module));
        assert_eq!(value.parent(), Some(wrapper));
        let head = exact(tree, source, "value_declaration_left", "following");
        assert_eq!(head.parent(), Some(value));
        let body = value.child_by_field_name("body").unwrap();
        written(body, source, "const", "30");
        assert_eq!(body.parent(), Some(value));
        assert_eq!(children(value), vec![head, body]);
        wrapper
    }
}

fn queries(tree: &Tree, source: &str, signature: bool, cases: &[Node<'_>]) {
    let expected_infix: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == "infix_expression")
        .collect();
    let mut patterns =
        "(enum_type_case) @case\n(enum_type_case (identifier) @name)\n(paren_expression) @paren"
            .to_owned();
    if !signature {
        patterns.push_str("\n(_expression) @expression");
    }
    if !expected_infix.is_empty() {
        patterns.push_str("\n(infix_expression) @infix");
    }
    let query = Query::new(&language(signature), &patterns).unwrap();
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(128);
    let mut case_captures = Vec::new();
    let mut names = Vec::new();
    let mut expressions = Vec::new();
    let mut parens = Vec::new();
    let mut infix = Vec::new();
    {
        let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
        while let Some((found, index)) = captures.next() {
            let capture = found.captures[*index];
            match query.capture_names()[usize::try_from(capture.index).unwrap()] {
                "case" => case_captures.push(capture.node),
                "name" => names.push(capture.node),
                "expression" => expressions.push(capture.node),
                "paren" => parens.push(capture.node),
                "infix" => infix.push(capture.node),
                other => panic!("unexpected query capture {other}"),
            }
        }
    }
    assert!(!cursor.did_exceed_match_limit());
    assert_eq!(case_captures, cases);
    assert_eq!(
        names,
        cases
            .iter()
            .map(|node| children(*node)[0])
            .collect::<Vec<_>>()
    );
    let expected_parens: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == "paren_expression")
        .collect();
    assert_eq!(parens, expected_parens);
    assert_eq!(infix, expected_infix);
    // Signature-only aliases must not widen ordinary signature expressions.
    if !signature {
        for node in expected_infix {
            assert_eq!(
                expressions
                    .iter()
                    .filter(|capture| **capture == node)
                    .count(),
                1,
                "expression ancestry: {}",
                node.to_sexp()
            );
        }
    }
}

fn clean(tree: &Tree, source: &str, signature: bool, expected: &[(&str, &str)]) {
    snapshot(tree, source);
    assert!(
        !tree.root_node().has_error(),
        "{source:?}: {}",
        tree.root_node().to_sexp()
    );
    assert!(
        nodes(tree)
            .iter()
            .all(|node| !node.is_error() && !node.is_missing())
    );
    let found: Vec<_> = expected
        .iter()
        .map(|(name, rhs)| case(tree, source, name, rhs))
        .collect();
    let block = found[0].parent().unwrap();
    assert_eq!(children(block), found);
    let definition = block.parent().unwrap();
    assert_eq!(definition.kind(), "enum_type_defn");
    assert_eq!(definition.child_by_field_name("block"), Some(block));
    let wrapper = definition.parent().unwrap();
    assert_eq!(wrapper.kind(), "type_definition");
    let later = following(tree, source, signature);
    assert_eq!(wrapper.parent(), later.parent());
    assert_eq!(block.end_byte(), found.last().unwrap().end_byte());
    assert!(definition.end_byte() <= later.start_byte());
    assert!(
        !nodes(tree)
            .iter()
            .any(|node| matches!(node.kind(), "union_type_case" | "type_abbrev_defn"))
    );
    let bars: Vec<_> = (0..block.child_count())
        .map(|index| block.child(u32::try_from(index).unwrap()).unwrap())
        .filter(|node| node.kind() == "|")
        .collect();
    assert_eq!(bars.len(), found.len());
    for (index, bar) in bars.iter().enumerate() {
        written(*bar, source, "|", "|");
        assert!(!bar.is_named() && !bar.is_extra());
        assert_eq!(bar.parent(), Some(block));
        assert!(bar.end_byte() <= found[index].start_byte());
        if index > 0 {
            assert!(found[index - 1].end_byte() < bar.start_byte());
        }
    }
    for node in nodes(tree).into_iter().filter(|node| {
        matches!(
            node.kind(),
            "paren_expression" | "infix_expression" | "enum_type_case"
        )
    }) {
        assert!(node.end_byte() < later.start_byte());
    }
    queries(tree, source, signature, &found);
}

fn infix<'tree>(
    tree: &'tree Tree,
    source: &str,
    expected: &str,
    left: &str,
    operator: &str,
    right: &str,
) -> Node<'tree> {
    let node = exact(tree, source, "infix_expression", expected);
    let direct = children(node);
    assert_eq!(direct.len(), 3);
    for (child, kind, expected) in [
        (direct[0], "const", left),
        (direct[1], "infix_op", operator),
        (direct[2], "const", right),
    ] {
        written(child, source, kind, expected);
        assert_eq!(child.parent(), Some(node));
    }
    assert_eq!(node.start_byte(), direct[0].start_byte());
    assert_eq!(node.end_byte(), direct[2].end_byte());
    node
}

fn recovery(tree: &Tree, source: &str) {
    snapshot(tree, source);
    assert!(
        tree.root_node().has_error(),
        "invalid frontier parsed clean: {source:?}"
    );
    let sexp = tree.root_node().to_sexp();
    assert!(
        sexp.contains("(ERROR") || sexp.contains("(MISSING "),
        "{sexp}"
    );
}

fn no_fabricated_cases(tree: &Tree, source: &str, allowed: &[&str]) {
    assert!(
        !nodes(tree)
            .iter()
            .any(|node| node.kind() == "union_type_case")
    );
    for node in nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == "enum_type_case" && !node.has_error())
    {
        let direct = children(node);
        assert_eq!(direct.len(), 2);
        assert_eq!(direct[0].kind(), "identifier");
        let name = text(direct[0], source);
        assert!(allowed.contains(&name), "fabricated case {name:?}");
        let starts: Vec<_> = source
            .match_indices(&format!("{name} ="))
            .map(|(offset, _)| offset)
            .collect();
        assert_eq!(starts, vec![node.start_byte()]);
        assert_eq!(node.end_byte(), direct[1].end_byte());
        if name == "Next" {
            written(direct[1], source, "const", "2");
        }
    }
}

#[test]
fn compiler_oracle_parentheses_shifts_and_comments_keep_direct_rhs_and_next_case() {
    for signature in [false, true] {
        for (fixture, multiline) in [
            (
                if signature {
                    include_str!("../fixtures/fsharp-enum-atomic-rhs/enum-literal-shift.fsi")
                } else {
                    include_str!("../fixtures/fsharp-enum-atomic-rhs/enum-literal-shift.fs")
                },
                false,
            ),
            (
                if signature {
                    include_str!("../fixtures/fsharp-enum-atomic-rhs/enum-multiline-comment.fsi")
                } else {
                    include_str!("../fixtures/fsharp-enum-atomic-rhs/enum-multiline-comment.fs")
                },
                true,
            ),
        ] {
            variants(fixture, |source| {
                let tree = parse(&mut parser(signature), source, None);
                let first_rhs = if multiline {
                    "(1u (* shift operand *)\n         <<< 0)"
                } else {
                    "(1u)"
                }
                .replace('\n', if source.contains('\r') { "\r\n" } else { "\n" });
                let next_rhs = if multiline { "(2u)" } else { "(1u <<< 0)" };
                clean(
                    &tree,
                    source,
                    signature,
                    &[("First", &first_rhs), ("Next", next_rhs)],
                );
                let shifted = if multiline {
                    "1u (* shift operand *)\n         <<< 0"
                        .replace('\n', if source.contains('\r') { "\r\n" } else { "\n" })
                } else {
                    "1u <<< 0".to_owned()
                };
                let expression = infix(&tree, source, &shifted, "1u", "<<<", "0");
                let paren = exact(
                    &tree,
                    source,
                    "paren_expression",
                    if multiline { &first_rhs } else { next_rhs },
                );
                assert_eq!(expression.parent(), Some(paren));
                assert_eq!(children(paren), vec![expression]);
                if multiline {
                    let comment = exact(&tree, source, "block_comment", "(* shift operand *)");
                    assert!(comment.is_extra());
                    assert!(
                        expression.start_byte() < comment.start_byte()
                            && comment.end_byte() < expression.end_byte()
                    );
                    for node in nodes(&tree) {
                        for field in ["name", "value", "body", "block"] {
                            assert_ne!(node.child_by_field_name(field), Some(comment));
                        }
                    }
                }
            });
        }
    }
}

#[test]
fn bare_constants_and_nested_parentheses_preserve_canonical_public_nodes() {
    for signature in [false, true] {
        for (body, rhs) in [
            ("    | First = -1\n    | Next = 2", "-1"),
            ("    | First = ((1u <<< 0))\n    | Next = 2", "((1u <<< 0))"),
        ] {
            variants(&fixture(signature, body), |source| {
                let tree = parse(&mut parser(signature), source, None);
                clean(&tree, source, signature, &[("First", rhs), ("Next", "2")]);
                if rhs.starts_with('(') {
                    let outer = exact(&tree, source, "paren_expression", rhs);
                    let inner = exact(&tree, source, "paren_expression", "(1u <<< 0)");
                    let expression = infix(&tree, source, "1u <<< 0", "1u", "<<<", "0");
                    assert_eq!(children(outer), vec![inner]);
                    assert_eq!(inner.parent(), Some(outer));
                    assert_eq!(children(inner), vec![expression]);
                    assert_eq!(expression.parent(), Some(inner));
                }
            });
        }
    }
}

#[test]
fn incomplete_or_misplaced_atomic_rhs_have_explicit_recovery_not_fabricated_cases() {
    for signature in [false, true] {
        for broken in ["1u<<<0", "1 ^ 2", "", "(1u<<<)", "(1u<<<0", "(1u |)"] {
            let source = format!(
                "module TypeExample\ntype Flags = | Broken = {broken} | Next = 2\ntype Later = int\n{}\n",
                if signature {
                    "val following: int"
                } else {
                    "let following = 30"
                }
            );
            variants(&source, |source| {
                let tree = parse(&mut parser(signature), source, None);
                recovery(&tree, source);
                no_fabricated_cases(&tree, source, &["Broken", "Next"]);
            });
        }
    }
}

#[test]
fn malformed_following_declarations_do_not_extend_completed_enum_values() {
    for signature in [false, true] {
        for tail in [
            "type Later =",
            if signature {
                "val following:"
            } else {
                "let following ="
            },
        ] {
            let fixture = format!(
                "module TypeExample\ntype Flags =\n    | First = (1u <<< 0)\n    | Next = (2u)\n{tail}\n"
            );
            variants(&fixture, |source| {
                let tree = parse(&mut parser(signature), source, None);
                recovery(&tree, source);
                let first = case(&tree, source, "First", "(1u <<< 0)");
                let next = case(&tree, source, "Next", "(2u)");
                assert!(!first.has_error() && !next.has_error());
                assert_eq!(first.parent(), next.parent());
                let tail_start = source.find(tail).unwrap();
                for node in nodes(&tree).into_iter().filter(|node| {
                    matches!(
                        node.kind(),
                        "enum_type_case" | "paren_expression" | "infix_expression"
                    )
                }) {
                    assert!(node.end_byte() < tail_start);
                }
            });
        }
    }
}

#[test]
fn aliases_unions_and_restricted_signature_value_bodies_do_not_gain_enum_syntax() {
    for signature in [false, true] {
        for (declaration, kind, union_count) in [
            ("type Alias = uint32", "type_abbrev_defn", 0),
            (
                "type Choice = | Empty | Item of uint32",
                "union_type_defn",
                2,
            ),
        ] {
            let source = format!(
                "module TypeExample\n{declaration}\n{}\n",
                if signature {
                    "val following: int"
                } else {
                    "let following = 30"
                }
            );
            variants(&source, |source| {
                let tree = parse(&mut parser(signature), source, None);
                snapshot(&tree, source);
                assert!(!tree.root_node().has_error());
                assert_eq!(
                    nodes(&tree)
                        .iter()
                        .filter(|node| node.kind() == kind)
                        .count(),
                    1
                );
                assert_eq!(
                    nodes(&tree)
                        .iter()
                        .filter(|node| node.kind() == "union_type_case")
                        .count(),
                    union_count
                );
                assert!(
                    !nodes(&tree)
                        .iter()
                        .any(|node| node.kind() == "enum_type_case")
                );
                following(&tree, source, signature);
            });
        }
    }
    for rhs in ["(1u<<<1)", "(7u&&&3u)", "(1 ^ 2)"] {
        variants(
            &format!("module TypeExample\nval value: uint32 = {rhs}\nval following: int\n"),
            |source| {
                let tree = parse(&mut parser(true), source, None);
                recovery(&tree, source);
                assert!(
                    !nodes(&tree)
                        .iter()
                        .any(|node| node.kind() == "enum_type_case")
                );
            },
        );
    }
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
fn length_changing_rhs_and_missing_close_edit_restore_match_complete_fresh_trees() {
    for signature in [false, true] {
        let original = fixture(signature, "    | First = (1u <<< 0)\n    | Next = 2");
        variants(&original, |source| {
            let mut reused = parser(signature);
            let mut old = parse(&mut reused, source, None);
            clean(
                &old,
                source,
                signature,
                &[("First", "(1u <<< 0)"), ("Next", "2")],
            );
            let original_snapshot = snapshot(&old, source);
            for replacement in ["7u", "(1u <<< 0"] {
                let start = source.find("(1u <<< 0)").unwrap();
                let old_end = start + "(1u <<< 0)".len();
                let mut changed = source.to_owned();
                changed.replace_range(start..old_end, replacement);
                edit(
                    &mut old,
                    source,
                    &changed,
                    start,
                    old_end,
                    start + replacement.len(),
                );
                let mut incremental = parse(&mut reused, &changed, Some(&old));
                let fresh = parse(&mut parser(signature), &changed, None);
                assert_eq!(snapshot(&incremental, &changed), snapshot(&fresh, &changed));
                if replacement == "7u" {
                    clean(
                        &incremental,
                        &changed,
                        signature,
                        &[("First", "7u"), ("Next", "2")],
                    );
                } else {
                    recovery(&incremental, &changed);
                    no_fabricated_cases(&incremental, &changed, &["First", "Next"]);
                }
                edit(
                    &mut incremental,
                    &changed,
                    source,
                    start,
                    start + replacement.len(),
                    old_end,
                );
                old = parse(&mut reused, source, Some(&incremental));
                let restored_fresh = parse(&mut parser(signature), source, None);
                assert_eq!(snapshot(&old, source), snapshot(&restored_fresh, source));
                assert_eq!(snapshot(&old, source), original_snapshot);
                clean(
                    &old,
                    source,
                    signature,
                    &[("First", "(1u <<< 0)"), ("Next", "2")],
                );
            }
        });
    }
}

#[test]
fn existing_bare_shift_recovery_keeps_broken_constant_next_boundary_and_following() {
    for (fixture, boundary) in [
        (
            "module Example\ntype Sample =\n    | Broken = 1u<<<0\n    | Next = 2u\nlet following = 30\n",
            false,
        ),
        (
            "module Example\ntype Sample = Broken = 1u<<<0 | Next = 2u\nlet following = 30\n",
            false,
        ),
        (
            "module Example\ntype Sample = Broken = 1u<<<0 | Next = 2u\ntype Boundary = int\nlet following = 30\n",
            true,
        ),
    ] {
        variants(fixture, |source| {
            let tree = parse(&mut parser(false), source, None);
            recovery(&tree, source);
            let broken = case(&tree, source, "Broken", "1u");
            let next = case(&tree, source, "Next", "2u");
            let block = broken.parent().unwrap();
            assert_eq!(next.parent(), Some(block));
            let cases: Vec<_> = nodes(&tree)
                .into_iter()
                .filter(|node| node.kind() == "enum_type_case")
                .collect();
            assert_eq!(cases, vec![broken, next]);
            assert_eq!(block.end_byte(), next.end_byte());
            let definition = block.parent().unwrap();
            assert_eq!(definition.kind(), "enum_type_defn");
            assert_eq!(
                nodes(&tree)
                    .iter()
                    .filter(|node| node.kind() == "enum_type_defn")
                    .count(),
                1
            );
            assert_eq!(definition.child_by_field_name("block"), Some(block));
            let wrapper = definition.parent().unwrap();
            assert_eq!(wrapper.kind(), "type_definition");
            let following = following(&tree, source, false);
            assert_eq!(wrapper.parent(), following.parent());
            assert!(wrapper.end_byte() < following.start_byte());
            let mut protected = vec![next, following];
            if boundary {
                let boundary = exact(&tree, source, "type_definition", "type Boundary = int");
                let alias = exact(&tree, source, "type_abbrev_defn", "Boundary = int");
                assert_eq!(alias.parent(), Some(boundary));
                assert_eq!(boundary.parent(), following.parent());
                assert!(
                    wrapper.end_byte() < boundary.start_byte()
                        && boundary.end_byte() < following.start_byte()
                );
                protected.push(boundary);
            }
            let shift = source.find("<<<0").unwrap();
            assert!(broken.end_byte() <= shift && shift + 4 < next.start_byte());
            let errors: Vec<_> = nodes(&tree)
                .into_iter()
                .filter(|node| node.is_error())
                .collect();
            assert!(!errors.is_empty());
            assert!(
                errors
                    .iter()
                    .any(|node| node.start_byte() < shift + 4 && shift < node.end_byte())
            );
            for error in errors {
                assert_eq!(error.parent(), Some(block));
                for node in &protected {
                    assert!(
                        error.end_byte() <= node.start_byte()
                            || node.end_byte() <= error.start_byte()
                    );
                }
            }
            assert!(
                !nodes(&tree)
                    .iter()
                    .any(|node| node.kind() == "union_type_case")
            );
        });
    }
}
