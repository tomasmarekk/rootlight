//! Native syntax controls for flat F# sequencing and genuine nested owners.
//! Literal visible-tree depth uses the root-at-zero provider convention; ranges,
//! query ancestry and incremental equivalence do not imply compiler semantics.

use std::{
    ops::{ControlFlow, Range},
    time::{Duration, Instant},
};

use tree_sitter::{
    InputEdit, Node, ParseOptions, Parser, Point, Query, QueryCursor, StreamingIterator, Tree,
};

const COUNTS: [usize; 5] = [1, 16, 64, 160, 320];
const FOLLOWING: &str = "let following = 30";

fn parser(signature: bool) -> Parser {
    let mut parser = Parser::new();
    let language = if signature {
        tree_sitter_fsharp::LANGUAGE_SIGNATURE
    } else {
        tree_sitter_fsharp::LANGUAGE_FSHARP
    };
    parser
        .set_language(&language.into())
        .expect("pinned grammar is incompatible with Tree-sitter");
    parser
}

fn parse(parser: &mut Parser, source: &str, old: Option<&Tree>) -> Tree {
    assert!(source.len() <= 16 * 1_024);
    let started = Instant::now();
    let mut checks = 0;
    let mut watchdog = |_: &tree_sitter::ParseState| {
        checks += 1;
        if checks > 20_000 || started.elapsed() > Duration::from_secs(10) {
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
        .expect("authored sequence exceeded its parse watchdog")
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

fn nodes(tree: &Tree) -> Vec<(Node<'_>, usize)> {
    let mut pending = vec![(tree.root_node(), 0)];
    let mut found = Vec::new();
    while let Some((node, depth)) = pending.pop() {
        assert!(
            found.len() < 20_000,
            "authored sequence exceeds its node bound"
        );
        found.push((node, depth));
        for index in (0..node.child_count()).rev() {
            pending.push((
                node.child(u32::try_from(index).unwrap()).unwrap(),
                depth + 1,
            ));
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
    let found: Vec<_> = nodes(tree)
        .into_iter()
        .map(|(node, _)| node)
        .filter(|node| node.kind() == kind && text(*node, source) == written)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{kind} {written:?}: {}",
        tree.root_node().to_sexp()
    );
    span(found[0], source, written);
    found[0]
}

fn named_operands(node: Node<'_>) -> Vec<Node<'_>> {
    (0..node.named_child_count())
        .map(|index| node.named_child(u32::try_from(index).unwrap()).unwrap())
        .filter(|child| !child.is_extra())
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    kind: String,
    bytes: Range<usize>,
    start: Point,
    end: Point,
    depth: usize,
    parent: Option<(String, Range<usize>)>,
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
        .map(|(node, depth)| {
            assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
            assert_eq!(node.start_position(), point(source, node.start_byte()));
            assert_eq!(node.end_position(), point(source, node.end_byte()));
            Snapshot {
                kind: node.kind().to_owned(),
                bytes: node.byte_range(),
                start: node.start_position(),
                end: node.end_position(),
                depth,
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
    (tree.root_node().to_sexp(), rows)
}

fn clean(tree: &Tree, source: &str) {
    assert_eq!(tree.root_node().kind(), "file");
    assert_eq!(tree.root_node().end_byte(), source.len());
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert!(
        nodes(tree)
            .iter()
            .all(|(node, _)| !node.is_error() && !node.is_missing())
    );
    snapshot(tree, source);
}

fn variants(body: &str, mut check: impl FnMut(&str, &str)) {
    let source = format!("module FlatSequenceExample\nlet run () =\n    {body}\n{FOLLOWING}\n");
    for newline in ["\n", "\r\n"] {
        for final_newline in [false, true] {
            let base = if final_newline {
                source.as_str()
            } else {
                source.strip_suffix('\n').unwrap()
            };
            check(&base.replace('\n', newline), &body.replace('\n', newline));
        }
    }
}

fn body<'tree>(tree: &'tree Tree, source: &str, written: &str) -> Node<'tree> {
    let binding_text = format!(
        "let run () ={}    {written}",
        if source.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        }
    );
    let binding = exact(tree, source, "function_or_value_defn", &binding_text);
    let expression = binding.child_by_field_name("body").unwrap();
    span(expression, source, written);
    assert_eq!(expression.parent(), Some(binding));
    let declaration = binding.parent().unwrap();
    assert_eq!(declaration.kind(), "declaration_expression");
    let owner = declaration.parent().unwrap();
    assert_eq!(owner.kind(), "named_module");
    span(
        owner.child_by_field_name("name").unwrap(),
        source,
        "FlatSequenceExample",
    );
    let following = exact(tree, source, "function_or_value_defn", FOLLOWING);
    let following_declaration = following.parent().unwrap();
    assert_eq!(following_declaration.kind(), "declaration_expression");
    assert_eq!(following_declaration.parent(), Some(owner));
    assert_ne!(declaration, following_declaration);
    span(following.child_by_field_name("body").unwrap(), source, "30");
    assert!(expression.end_byte() < following.start_byte());
    assert!(declaration.end_byte() <= following_declaration.start_byte());
    expression
}

fn expression_queries(tree: &Tree, source: &str, operands: &[Node<'_>]) -> usize {
    let language = tree_sitter_fsharp::LANGUAGE_FSHARP.into();
    let mut kinds: Vec<_> = operands.iter().map(|node| node.kind()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    // These contextual families replace literal _expression ancestry only in
    // this private shadow. Concrete app/sequence selectors cover documented
    // aliases; raw const, identifier and unit kinds are not fallback selectors.
    let mut pattern = String::from(
        "[(_expression) (_atomic_expression) (_arg_expression) \
         (_module_expression) (_application_operand/ce_expression) (application_expression) (sequential_expression) (typecast_expression)] @generic\n",
    );
    for kind in kinds {
        pattern.push_str(&format!("({kind}) @explicit\n"));
    }
    let query = Query::new(&language, &pattern)
        .expect("contextual expression union and explicit operand queries must compile");
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(1_024);
    let mut generic = Vec::new();
    let mut explicit = Vec::new();
    let mut count = 0;
    {
        let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
        while let Some((found, index)) = captures.next() {
            count += 1;
            assert!(count < 20_000);
            let capture = found.captures[*index];
            match query.capture_names()[usize::try_from(capture.index).unwrap()] {
                "generic" => generic.push(capture.node),
                "explicit" => explicit.push(capture.node),
                other => panic!("unexpected capture {other}"),
            }
        }
    }
    assert!(!cursor.did_exceed_match_limit());
    let mut walked = std::collections::BTreeMap::<usize, Vec<Node<'_>>>::new();
    for (node, _) in nodes(tree) {
        walked.entry(node.id()).or_default().push(node);
    }
    let identity = |node: Node<'_>| {
        (
            node.id(),
            node.kind().to_owned(),
            (node.start_byte(), node.end_byte()),
            node.parent().map(|parent| {
                (
                    parent.id(),
                    parent.kind().to_owned(),
                    (parent.start_byte(), parent.end_byte()),
                )
            }),
            node.is_named(),
            node.is_extra(),
            node.is_error(),
            node.is_missing(),
        )
    };
    for capture in generic.iter().chain(&explicit) {
        assert_eq!(
            walked
                .get(&capture.id())
                .unwrap()
                .iter()
                .filter(|node| identity(**node) == identity(*capture))
                .count(),
            1,
            "query capture must join exactly one walked node and direct parent"
        );
        span(*capture, source, text(*capture, source));
    }
    let mut raw_identities = std::collections::BTreeSet::new();
    for capture in &generic {
        // A repeated union event is evidence, not permission to normalize it.
        assert!(
            raw_identities.insert(identity(*capture)),
            "raw contextual selector captured the same node more than once"
        );
        assert!(
            !matches!(
                capture.kind(),
                "unit"
                    | "identifier"
                    | "long_identifier"
                    | "argument_patterns"
                    | "identifier_pattern"
                    | "typed_pattern"
                    | "simple_type"
            ),
            "binding, type and raw unit syntax are not expression nodes"
        );
        let mut ancestor = capture.parent();
        while let Some(parent) = ancestor {
            if parent.kind() == "function_or_value_defn" {
                let body = parent.child_by_field_name("body").unwrap();
                assert!(
                    body.start_byte() <= capture.start_byte()
                        && capture.end_byte() <= body.end_byte(),
                    "authored binding-head syntax was captured as an expression"
                );
            }
            if parent.kind() == "rule"
                && let Some(pattern) = parent.child_by_field_name("pattern")
            {
                assert!(
                    capture.end_byte() <= pattern.start_byte()
                        || capture.start_byte() >= pattern.end_byte(),
                    "authored match-pattern syntax was captured as an expression"
                );
            }
            ancestor = parent.parent();
        }
    }
    for operand in operands {
        assert_eq!(
            generic.iter().filter(|node| **node == *operand).count(),
            1,
            "{} lost contextual expression ancestry",
            operand.to_sexp()
        );
        assert_eq!(
            explicit.iter().filter(|node| **node == *operand).count(),
            1,
            "{} lost explicit query ownership",
            operand.to_sexp()
        );
    }
    generic.len()
}

fn flat<'tree>(
    tree: &'tree Tree,
    source: &str,
    expression: Node<'tree>,
    expected: &[(&str, &str)],
) {
    assert_eq!(expression.kind(), "sequential_expression");
    let children = named_operands(expression);
    assert_eq!(children.len(), expected.len(), "{}", expression.to_sexp());
    for (child, (kind, written)) in children.iter().zip(expected) {
        assert_eq!(child.kind(), *kind);
        span(*child, source, written);
        assert_eq!(child.parent(), Some(expression));
        assert_ne!(child.kind(), "sequential_expression");
    }
    for pair in children.windows(2) {
        assert!(pair[0].end_byte() < pair[1].start_byte());
    }
    assert_eq!(
        expression.start_byte(),
        children.first().unwrap().start_byte()
    );
    assert_eq!(expression.end_byte(), children.last().unwrap().end_byte());
    expression_queries(tree, source, &children);
}

#[test]
fn flat_layout_repetitions_have_literal_constant_ast_depth_and_one_sequence_owner() {
    for count in COUNTS {
        let mut written = String::new();
        for _ in 0..count {
            written.push_str("ignore 0\n    ");
        }
        written.push_str("()");
        variants(&written, |source, written| {
            let tree = parse(&mut parser(false), source, None);
            clean(&tree, source);
            // This is the literal measured visible-tree depth, not depth with
            // sequence wrappers discounted or a baseline subtracted.
            assert_eq!(
                nodes(&tree).iter().map(|(_, depth)| *depth).max(),
                Some(7),
                "repetitions={count}"
            );
            let expression = body(&tree, source, written);
            assert_eq!(
                nodes(&tree)
                    .iter()
                    .filter(|(node, _)| node.kind() == "sequential_expression")
                    .count(),
                1
            );
            let mut expected = vec![("application_expression", "ignore 0"); count];
            expected.push(("const", "()"));
            flat(&tree, source, expression, &expected);
            let children = named_operands(expression);
            let written_ranges: Vec<_> = source
                .match_indices("ignore 0")
                .map(|(start, text)| start..start + text.len())
                .collect();
            assert_eq!(
                children[..count]
                    .iter()
                    .map(|node| node.byte_range())
                    .collect::<Vec<_>>(),
                written_ranges
            );
            let unit_start = source.rfind("    ()").unwrap() + 4;
            assert_eq!(children[count].byte_range(), unit_start..unit_start + 2);
        });
    }
}

#[test]
fn single_operand_remains_its_own_expression_without_sequence_wrapper() {
    variants("ignore 0", |source, written| {
        let tree = parse(&mut parser(false), source, None);
        clean(&tree, source);
        let expression = body(&tree, source, written);
        assert_eq!(expression.kind(), "application_expression");
        assert!(
            !nodes(&tree)
                .iter()
                .any(|(node, _)| node.kind() == "sequential_expression")
        );
        expression_queries(&tree, source, &[expression]);
    });
}

#[test]
fn explicit_separators_and_trivia_keep_flat_order_and_following_boundary() {
    for body_text in [
        "ignore 11; ignore 12; ignore 13; ()",
        "ignore 11 // trailing\n\n    (* outer (* nested *) tail *)\n    ignore 12\n    // between\n    ignore 13\n    ()",
        "ignore 11; // trailing\n\n    ignore 12;\n    ignore 13\n    ()",
    ] {
        variants(body_text, |source, written| {
            let tree = parse(&mut parser(false), source, None);
            clean(&tree, source);
            let expression = body(&tree, source, written);
            flat(
                &tree,
                source,
                expression,
                &[
                    ("application_expression", "ignore 11"),
                    ("application_expression", "ignore 12"),
                    ("application_expression", "ignore 13"),
                    ("const", "()"),
                ],
            );
            assert_eq!(
                nodes(&tree)
                    .iter()
                    .filter(|(node, _)| node.kind() == "sequential_expression")
                    .count(),
                1
            );
            if written.contains("//") {
                assert!(
                    nodes(&tree)
                        .iter()
                        .any(|(node, _)| node.kind() == "line_comment" && node.is_extra())
                );
            }
            if written.contains("(*") {
                let comment = exact(
                    &tree,
                    source,
                    "block_comment",
                    "(* outer (* nested *) tail *)",
                );
                assert!(comment.is_extra());
            }
            if written.contains(';') {
                assert!(
                    nodes(&tree)
                        .iter()
                        .any(|(node, _)| node.kind() == ";" && text(*node, source) == ";")
                );
            }
        });
    }
}

fn nested_owner(source: &str, written: &str, kind: &str, probes: &[(&str, &str)]) {
    let tree = parse(&mut parser(false), source, None);
    clean(&tree, source);
    let expression = body(&tree, source, written);
    let start = written.find('\n').unwrap() + 5;
    let newline = if written.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let end = written.rfind(&format!("{newline}    ignore 103")).unwrap();
    let owner_text = written.get(start..end).unwrap();
    let owner = exact(&tree, source, kind, owner_text);
    flat(
        &tree,
        source,
        expression,
        &[
            ("application_expression", "ignore 101"),
            (kind, owner_text),
            ("application_expression", "ignore 103"),
            ("const", "()"),
        ],
    );
    assert_eq!(owner.parent(), Some(expression));
    for (probe_kind, probe_text) in probes {
        let probe = exact(&tree, source, probe_kind, probe_text);
        assert!(
            std::iter::successors(probe.parent(), |node| node.parent()).any(|node| node == owner),
            "{} escaped {}",
            probe.to_sexp(),
            owner.to_sexp()
        );
        assert_ne!(probe.parent(), Some(expression));
    }
    for (sequence, _) in nodes(&tree)
        .iter()
        .filter(|(node, _)| node.kind() == "sequential_expression")
    {
        if *sequence != expression {
            assert!(
                std::iter::successors(sequence.parent(), |node| node.parent())
                    .any(|node| node == owner)
            );
            let operands = named_operands(*sequence);
            assert!(operands.len() >= 2);
            assert!(
                operands.iter().all(|node| node.parent() == Some(*sequence)
                    && node.kind() != "sequential_expression")
            );
            expression_queries(&tree, source, &operands);
        }
    }
}

#[test]
fn genuine_nested_scope_owners_do_not_release_their_operands_into_outer_sequence() {
    for (kind, owner_text, probes) in [
        (
            "paren_expression",
            "(ignore 201; (ignore 202; ()))",
            vec![
                ("application_expression", "ignore 201"),
                ("application_expression", "ignore 202"),
            ],
        ),
        (
            "paren_expression",
            "(let local = 201 in ignore local; ())",
            vec![
                ("function_or_value_defn", "let local = 201 in"),
                ("application_expression", "ignore local"),
            ],
        ),
        (
            "if_expression",
            "if ready then\n        ignore 201\n        ()\n    else\n        ignore 202\n        ()",
            vec![
                ("application_expression", "ignore 201"),
                ("application_expression", "ignore 202"),
            ],
        ),
        (
            "match_expression",
            "match value with\n    | 0 ->\n        ignore 201\n        ()\n    | _ ->\n        ignore 202\n        ()",
            vec![
                ("application_expression", "ignore 201"),
                ("application_expression", "ignore 202"),
            ],
        ),
        (
            "for_expression",
            "for index in values do\n        ignore 201\n        ignore 202\n        ()",
            vec![
                ("application_expression", "ignore 201"),
                ("application_expression", "ignore 202"),
            ],
        ),
        (
            "while_expression",
            "while ready do\n        ignore 201\n        ignore 202\n        ()",
            vec![
                ("application_expression", "ignore 201"),
                ("application_expression", "ignore 202"),
            ],
        ),
        (
            "try_expression",
            "try\n        ignore 201\n        ()\n    finally\n        ignore 202",
            vec![
                ("application_expression", "ignore 201"),
                ("application_expression", "ignore 202"),
            ],
        ),
        (
            "ce_expression",
            "builder {\n        ignore 201\n        ignore 202\n        ()\n    }",
            vec![
                ("application_expression", "ignore 201"),
                ("application_expression", "ignore 202"),
            ],
        ),
        (
            "list_expression",
            "[\n        (ignore 201; 1)\n        (ignore 202; 2)\n    ]",
            vec![
                ("application_expression", "ignore 201"),
                ("application_expression", "ignore 202"),
            ],
        ),
        (
            "array_expression",
            "[|\n        (ignore 201; 1)\n        (ignore 202; 2)\n    |]",
            vec![
                ("application_expression", "ignore 201"),
                ("application_expression", "ignore 202"),
            ],
        ),
    ] {
        let written = format!("ignore 101\n    {owner_text}\n    ignore 103\n    ()");
        variants(&written, |source, written| {
            nested_owner(source, written, kind, &probes)
        });
    }
}

#[test]
fn multiline_application_and_infix_continuations_are_not_sequence_operands() {
    for (kind, owner_text) in [
        (
            "application_expression",
            "combine\n        201\n        202",
        ),
        ("infix_expression", "201 +\n        202"),
    ] {
        let written = format!("ignore 101\n    {owner_text}\n    ignore 103\n    ()");
        variants(&written, |source, written| {
            nested_owner(source, written, kind, &[("const", "201"), ("const", "202")])
        });
    }
}

fn edit(tree: &mut Tree, before: &str, after: &str) {
    let start = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = before.as_bytes()[start..]
        .iter()
        .rev()
        .zip(after.as_bytes()[start..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let old_end = before.len() - suffix;
    let new_end = after.len() - suffix;
    assert!(start < before.find(FOLLOWING).unwrap());
    assert!(old_end <= before.find(FOLLOWING).unwrap());
    assert!(new_end <= after.find(FOLLOWING).unwrap());
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
fn middle_operand_insert_delete_and_restore_match_exact_fresh_native_snapshots() {
    variants(
        "ignore 10\n    ignore 20\n    ignore 30\n    ()",
        |source, _| {
            let newline = if source.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            let mut reused = parser(false);
            let original = parse(&mut reused, source, None);
            clean(&original, source);
            let original_snapshot = snapshot(&original, source);
            let mut inserted = source.to_owned();
            inserted.insert_str(
                source.find("    ignore 30").unwrap(),
                &format!("    ignore 25{newline}"),
            );
            let deleted = inserted.replace(&format!("    ignore 20{newline}"), "");
            let mut before = source.to_owned();
            let mut current = original;
            for after in [inserted, deleted, source.to_owned()] {
                edit(&mut current, &before, &after);
                current = parse(&mut reused, &after, Some(&current));
                let fresh = parse(&mut parser(false), &after, None);
                clean(&current, &after);
                clean(&fresh, &after);
                assert_eq!(snapshot(&current, &after), snapshot(&fresh, &after));
                let body_start = after.find("    ignore 10").unwrap() + 4;
                let body_end = after.find(&format!("{newline}{FOLLOWING}")).unwrap();
                let expression = body(&current, &after, &after[body_start..body_end]);
                let children = named_operands(expression);
                assert_eq!(expression.kind(), "sequential_expression");
                assert!(
                    children
                        .iter()
                        .all(|node| node.kind() != "sequential_expression"
                            && node.parent() == Some(expression))
                );
                let ranges: Vec<_> = after
                    .match_indices("ignore ")
                    .map(|(start, _)| start..start + "ignore 10".len())
                    .collect();
                assert_eq!(children.len(), ranges.len() + 1);
                assert!(
                    children[..ranges.len()]
                        .iter()
                        .all(|node| node.kind() == "application_expression")
                );
                assert_eq!(
                    children[..ranges.len()]
                        .iter()
                        .map(|node| node.byte_range())
                        .collect::<Vec<_>>(),
                    ranges
                );
                assert_eq!(children.last().unwrap().kind(), "const");
                span(*children.last().unwrap(), &after, "()");
                assert_eq!(
                    nodes(&current).iter().map(|(_, depth)| *depth).max(),
                    Some(7)
                );
                assert_eq!(
                    nodes(&current)
                        .iter()
                        .filter(|(node, _)| node.kind() == "sequential_expression")
                        .count(),
                    1
                );
                expression_queries(&current, &after, &children);
                before = after;
            }
            assert_eq!(snapshot(&current, source), original_snapshot);
        },
    );
}

#[test]
fn signature_profile_keeps_value_signatures_without_admitting_sequence_bodies() {
    for source in [
        "module FlatSequenceExample\nval run : unit -> unit\nval following : int\n",
        "module FlatSequenceExample\nval run : unit -> unit =\n    ignore 0\n    ()\nval following : int\n",
    ] {
        for newline in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let base = if final_newline {
                    source
                } else {
                    source.strip_suffix('\n').unwrap()
                };
                let source = base.replace('\n', newline);
                let tree = parse(&mut parser(true), &source, None);
                snapshot(&tree, &source);
                assert_eq!(tree.root_node().end_byte(), source.len());
                assert!(
                    !nodes(&tree)
                        .iter()
                        .any(|(node, _)| node.kind() == "sequential_expression")
                );
                if source.contains('=') {
                    assert!(
                        tree.root_node().has_error(),
                        "signature admitted implementation sequence"
                    );
                } else {
                    clean(&tree, &source);
                    let run = exact(&tree, &source, "value_definition", "val run : unit -> unit");
                    let following =
                        exact(&tree, &source, "value_definition", "val following : int");
                    assert_eq!(run.parent(), following.parent());
                    assert_eq!(run.parent().unwrap().kind(), "named_module");
                    assert!(run.end_byte() < following.start_byte());
                }
            }
        }
    }
}

#[test]
fn double_semicolon_ends_the_binding_before_independent_module_expressions() {
    for (body_text, separate_units) in [
        ("ignore 11; ignore 12;; ignore 13; ()", true),
        (
            "ignore 11; // trailing\n\n    ignore 12;;\n    ignore 13\n    ()",
            false,
        ),
    ] {
        variants(body_text, |source, written| {
            let tree = parse(&mut parser(false), source, None);
            clean(&tree, source);
            let boundary = written.find(";;").unwrap();
            let prefix = &written[..boundary];
            let expression = body(&tree, source, prefix);
            flat(
                &tree,
                source,
                expression,
                &[
                    ("application_expression", "ignore 11"),
                    ("application_expression", "ignore 12"),
                ],
            );
            let binding = expression.parent().unwrap();
            let declaration = binding.parent().unwrap();
            let module = declaration.parent().unwrap();
            let module_name = module.child_by_field_name("name").unwrap();
            let following = exact(&tree, source, "function_or_value_defn", FOLLOWING);
            let following_declaration = following.parent().unwrap();
            let items: Vec<_> = named_operands(module)
                .into_iter()
                .filter(|node| *node != module_name)
                .collect();
            let boundary_start = source.find(";;").unwrap();
            assert_eq!(expression.end_byte(), boundary_start);
            assert_eq!(binding.end_byte(), boundary_start);
            assert_eq!(declaration.end_byte(), boundary_start);
            assert_eq!(items.first().copied(), Some(declaration));
            assert_eq!(items.last().copied(), Some(following_declaration));

            let ignore = exact(&tree, source, "application_expression", "ignore 13");
            let unit_start = source.rfind("()").unwrap();
            let units: Vec<_> = nodes(&tree)
                .into_iter()
                .map(|(node, _)| node)
                .filter(|node| {
                    node.kind() == "const" && node.byte_range() == (unit_start..unit_start + 2)
                })
                .collect();
            assert_eq!(units.len(), 1);
            let unit = units[0];
            span(unit, source, "()");
            assert!(ignore.start_byte() >= boundary_start + 2);
            assert!(unit.start_byte() > ignore.end_byte());
            assert!(unit.end_byte() < following.start_byte());
            if separate_units {
                assert_eq!(items.len(), 4);
                assert_eq!(items[1], ignore);
                assert_eq!(items[2], unit);
                assert_eq!(ignore.parent(), Some(module));
                assert_eq!(unit.parent(), Some(module));
                expression_queries(&tree, source, &[ignore, unit]);
            } else {
                // The compiler records one module Expr for this exact range,
                // not the inner expression kind or a synthetic do wrapper.
                assert_eq!(items.len(), 3);
                let module_expression = items[1];
                let newline = if source.contains("\r\n") {
                    "\r\n"
                } else {
                    "\n"
                };
                span(
                    module_expression,
                    source,
                    &format!("ignore 13{newline}    ()"),
                );
                assert_eq!(module_expression.parent(), Some(module));
                for descendant in [ignore, unit] {
                    let mut parent = descendant.parent();
                    let mut found = false;
                    for _ in 0..32 {
                        let Some(node) = parent else { break };
                        if node == module_expression {
                            found = true;
                            break;
                        }
                        parent = node.parent();
                    }
                    assert!(
                        found,
                        "{} escaped the independent module Expr",
                        descendant.to_sexp()
                    );
                }
                expression_queries(&tree, source, &[module_expression, ignore, unit]);
            }
            for item in &items[1..items.len() - 1] {
                assert!(item.start_byte() >= boundary_start + 2);
                assert!(item.end_byte() < following.start_byte());
                assert!(item.start_byte() >= expression.end_byte());
            }
            assert_eq!(
                nodes(&tree)
                    .iter()
                    .filter(|(node, _)| node.kind() == "function_or_value_defn")
                    .count(),
                2
            );
            if written.contains("//") {
                assert!(
                    nodes(&tree)
                        .iter()
                        .any(|(node, _)| node.kind() == "line_comment" && node.is_extra())
                );
            }
        });
    }
}

#[test]
fn contextual_expression_selector_excludes_binding_type_and_raw_unit_nodes() {
    fn at<'tree>(tree: &'tree Tree, source: &str, kind: &str, range: Range<usize>) -> Node<'tree> {
        let found: Vec<_> = nodes(tree)
            .into_iter()
            .map(|(node, _)| node)
            .filter(|node| node.kind() == kind && node.byte_range() == range)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "{kind} {range:?}: {}",
            tree.root_node().to_sexp()
        );
        span(found[0], source, &source[range]);
        found[0]
    }

    let typed = "module QueryBoundaryExample\nlet run (value: int) =\n    ignore 11\n    ignore 12\n    ()\nlet following = 30\n";
    let direct = "module QueryBoundaryExample\nlet run () = ignore()\nlet following = 30\n";
    for newline in ["\n", "\r\n"] {
        for (source_lf, typed_argument) in [(typed, true), (direct, false)] {
            let source = source_lf.replace('\n', newline);
            let tree = parse(&mut parser(false), &source, None);
            clean(&tree, &source);
            let following = exact(&tree, &source, "function_or_value_defn", FOLLOWING);
            let following_const = following.child_by_field_name("body").unwrap();
            assert_eq!(following_const.kind(), "const");
            span(following_const, &source, "30");
            let binding_start = source.find("let run").unwrap();
            let binding_end = source.find(&format!("{newline}let following")).unwrap();
            let binding = at(
                &tree,
                &source,
                "function_or_value_defn",
                binding_start..binding_end,
            );
            let expression = binding.child_by_field_name("body").unwrap();
            assert_eq!(expression.parent(), Some(binding));
            assert!(expression.end_byte() < following.start_byte());
            let declaration = binding.parent().unwrap();
            let following_declaration = following.parent().unwrap();
            assert_eq!(declaration.kind(), "declaration_expression");
            assert_eq!(following_declaration.kind(), "declaration_expression");
            let module = declaration.parent().unwrap();
            assert_eq!(module.kind(), "named_module");
            assert_eq!(following_declaration.parent(), Some(module));
            span(
                module.child_by_field_name("name").unwrap(),
                &source,
                "QueryBoundaryExample",
            );
            let mut expected = vec![expression, following_const];
            let mut negatives = Vec::new();
            if typed_argument {
                assert_eq!(expression.kind(), "sequential_expression");
                let body_text = format!("ignore 11{newline}    ignore 12{newline}    ()");
                span(expression, &source, &body_text);
                let operands = named_operands(expression);
                assert_eq!(operands.len(), 3);
                for (index, value) in ["11", "12"].into_iter().enumerate() {
                    let written = format!("ignore {value}");
                    let application = exact(&tree, &source, "application_expression", &written);
                    assert_eq!(operands[index], application);
                    assert_eq!(application.parent(), Some(expression));
                    let callee = at(
                        &tree,
                        &source,
                        "long_identifier_or_op",
                        application.start_byte()..application.start_byte() + "ignore".len(),
                    );
                    let argument = at(
                        &tree,
                        &source,
                        "const",
                        application.end_byte() - value.len()..application.end_byte(),
                    );
                    assert_eq!(callee.parent(), Some(application));
                    assert_eq!(argument.parent(), Some(application));
                    expected.extend([application, callee, argument]);
                }
                let body_unit = operands[2];
                assert_eq!(body_unit.kind(), "const");
                assert_eq!(body_unit.parent(), Some(expression));
                span(body_unit, &source, "()");
                expected.push(body_unit);
                negatives.push(at(&tree, &source, "unit", body_unit.byte_range()));
                let value_start = source.find("value: int").unwrap();
                let type_start = value_start + "value: ".len();
                let value = at(&tree, &source, "identifier", value_start..value_start + 5);
                let type_name = at(&tree, &source, "identifier", type_start..type_start + 3);
                let typed_pattern =
                    at(&tree, &source, "typed_pattern", value_start..type_start + 3);
                assert_eq!(value.parent().unwrap().kind(), "long_identifier_or_op");
                assert_eq!(type_name.parent().unwrap().kind(), "long_identifier");
                assert!(typed_pattern.start_byte() < expression.start_byte());
                negatives.extend([value, type_name, typed_pattern]);
                assert_eq!(expected.len(), 9);
            } else {
                assert_eq!(expression.kind(), "application_expression");
                span(expression, &source, "ignore()");
                let callee = at(
                    &tree,
                    &source,
                    "long_identifier_or_op",
                    expression.start_byte()..expression.start_byte() + "ignore".len(),
                );
                assert_eq!(callee.parent(), Some(expression));
                expected.push(callee);
                let call_unit = at(
                    &tree,
                    &source,
                    "unit",
                    expression.end_byte() - 2..expression.end_byte(),
                );
                assert_eq!(call_unit.parent(), Some(expression));
                let binder_start = source.find("()").unwrap();
                let binder_const = at(&tree, &source, "const", binder_start..binder_start + 2);
                let binder_unit = at(&tree, &source, "unit", binder_start..binder_start + 2);
                assert_eq!(binder_unit.parent(), Some(binder_const));
                assert!(binder_const.end_byte() < expression.start_byte());
                negatives.extend([call_unit, binder_const, binder_unit]);
                assert_eq!(expected.len(), 3);
            }
            // This complete fixture inventory is specified independently of
            // query results. Raw capture-one plus this count rejects every
            // extra binder, pattern, type, literal leaf or duplicate event.
            assert_eq!(
                expression_queries(&tree, &source, &expected),
                expected.len()
            );
            for negative in negatives {
                span(negative, &source, text(negative, &source));
                assert!(negative.parent().is_some());
                assert!(expected.iter().all(|node| {
                    node.id() != negative.id()
                        || node.kind() != negative.kind()
                        || node.byte_range() != negative.byte_range()
                }));
            }
        }
    }
}

// Native source ownership for a local binding closed by the hidden IN token.
// The binding includes written `in`; its value and continuation remain separate.

#[test]
fn hidden_in_closure_preserves_binding_span_body_and_continuation_ownership() {
    let local_text = "let local = 201 in";
    let declaration_text = "let local = 201 in ignore local; ()";
    let paren_text = "(let local = 201 in ignore local; ())";
    let written = format!("ignore 101\n    {paren_text}\n    ignore 103\n    ()");
    variants(&written, |source, written| {
        let tree = parse(&mut parser(false), source, None);
        clean(&tree, source);
        let expression = body(&tree, source, written);
        let paren = exact(&tree, source, "paren_expression", paren_text);
        let declaration = exact(&tree, source, "declaration_expression", declaration_text);
        let local = exact(&tree, source, "function_or_value_defn", local_text);
        let value = local.child_by_field_name("body").unwrap();
        let continuation = declaration.child_by_field_name("in").unwrap();
        let application = exact(&tree, source, "application_expression", "ignore local");
        let following = exact(&tree, source, "function_or_value_defn", FOLLOWING);
        let shifted = if source.contains("\r\n") { 3 } else { 0 };

        assert_eq!(local.byte_range(), 60 + shifted..78 + shifted);
        assert_eq!(value.kind(), "const");
        assert_eq!(value.byte_range(), 72 + shifted..75 + shifted);
        span(value, source, "201");
        assert_eq!(value.parent(), Some(local));
        assert_eq!(local.parent(), Some(declaration));
        assert_eq!(declaration.parent(), Some(paren));
        assert_eq!(named_operands(paren), vec![declaration]);
        assert_eq!(named_operands(declaration), vec![local, continuation]);
        assert_eq!(paren.parent(), Some(expression));
        assert_eq!(paren.byte_range(), 59 + shifted..96 + shifted);
        assert_eq!(declaration.byte_range(), 60 + shifted..95 + shifted);
        assert_eq!(continuation.kind(), "sequential_expression");
        assert_eq!(continuation.byte_range(), 79 + shifted..95 + shifted);
        span(continuation, source, "ignore local; ()");
        assert_eq!(continuation.parent(), Some(declaration));
        assert_eq!(application.parent(), Some(continuation));
        let continuation_operands = named_operands(continuation);
        assert_eq!(continuation_operands.len(), 2);
        assert_eq!(continuation_operands[0], application);
        let continuation_unit = continuation_operands[1];
        assert_eq!(continuation_unit.kind(), "const");
        span(continuation_unit, source, "()");
        assert_eq!(continuation_unit.parent(), Some(continuation));

        // IN is consumed by the block terminator, not published as a named body.
        assert_eq!(source.get(value.end_byte()..local.end_byte()), Some(" in"));
        assert_eq!(
            source.get(local.end_byte()..continuation.start_byte()),
            Some(" ")
        );
        assert!(local.end_byte() < continuation.start_byte());
        assert!(value.end_byte() < local.end_byte());
        assert!(paren.end_byte() < following.start_byte());

        let query = Query::new(
            &tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
            "(paren_expression (declaration_expression \
             (function_or_value_defn body: (const) @value) @binding \
             in: (sequential_expression) @continuation) @declaration) @owner",
        )
        .expect("pinned local-binding field query must compile");
        let mut cursor = QueryCursor::new();
        cursor.set_match_limit(128);
        let mut events = Vec::new();
        {
            let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
            while let Some((found, index)) = captures.next() {
                assert!(events.len() < 16);
                let capture = found.captures[*index];
                events.push((
                    query.capture_names()[usize::try_from(capture.index).unwrap()],
                    capture.node,
                ));
            }
        }
        assert!(!cursor.did_exceed_match_limit());
        assert_eq!(events.len(), 5);
        for (role, expected) in [
            ("owner", paren),
            ("declaration", declaration),
            ("binding", local),
            ("value", value),
            ("continuation", continuation),
        ] {
            assert_eq!(
                events
                    .iter()
                    .filter(|(name, node)| *name == role && *node == expected)
                    .count(),
                1,
            );
            span(expected, source, text(expected, source));
        }

        let before = exact(&tree, source, "application_expression", "ignore 101");
        let after = exact(&tree, source, "application_expression", "ignore 103");
        let mut expected = vec![
            expression,
            paren,
            declaration,
            value,
            continuation,
            application,
            continuation_unit,
            following.child_by_field_name("body").unwrap(),
        ];
        for application in [before, application, after] {
            if application != continuation_operands[0] {
                expected.push(application);
            }
            let children = named_operands(application);
            assert_eq!(children.len(), 2);
            assert_eq!(children[0].kind(), "long_identifier_or_op");
            assert_eq!(children[0].parent(), Some(application));
            assert_eq!(children[1].parent(), Some(application));
            expected.extend(children);
        }
        let outer_operands = named_operands(expression);
        assert_eq!(outer_operands.len(), 4);
        assert_eq!(outer_operands[0], before);
        assert_eq!(outer_operands[1], paren);
        assert_eq!(outer_operands[2], after);
        let outer_unit = outer_operands[3];
        assert_eq!(outer_unit.kind(), "const");
        span(outer_unit, source, "()");
        assert_eq!(outer_unit.parent(), Some(expression));
        expected.push(outer_unit);
        assert_eq!(expected.len(), 17);
        assert_eq!(expression_queries(&tree, source, &expected), expected.len());

        let binder = exact(&tree, source, "value_declaration_left", "local");
        let binder_pattern = exact(&tree, source, "identifier_pattern", "local");
        assert_eq!(binder.parent(), Some(local));
        assert_eq!(binder_pattern.parent(), Some(binder));
        for negative in nodes(&tree)
            .into_iter()
            .map(|(node, _)| node)
            .filter(|node| {
                node.kind() == "function_or_value_defn"
                    || node.kind() == "unit"
                    || (binder.start_byte() <= node.start_byte()
                        && node.end_byte() <= binder.end_byte())
            })
        {
            assert!(expected.iter().all(|node| *node != negative));
        }
        assert_eq!(
            following.parent().unwrap().parent(),
            expression.parent().unwrap().parent().unwrap().parent(),
        );
    });
}

// Exact compiler-qualified frontiers use native public ownership, not the
// compiler's desugared App counts or its braces-only ComputationExpr range.

include!("frontier-sources.rs");

fn frontier_children<'tree>(
    owner: Node<'tree>,
    source: &str,
    expected: &[(&str, &str)],
) -> Vec<Node<'tree>> {
    let children = named_operands(owner);
    assert_eq!(children.len(), expected.len(), "{}", owner.to_sexp());
    for (child, (kind, written)) in children.iter().zip(expected) {
        assert_eq!(child.kind(), *kind, "{}", owner.to_sexp());
        span(*child, source, written);
        assert_eq!(child.parent(), Some(owner));
        assert!(owner.start_byte() <= child.start_byte() && child.end_byte() <= owner.end_byte());
    }
    for pair in children.windows(2) {
        assert!(pair[0].end_byte() <= pair[1].start_byte());
    }
    children
}

fn frontier_application<'tree>(
    application: Node<'tree>,
    source: &str,
    callee: (&str, &str),
    argument: (&str, &str),
) -> [Node<'tree>; 2] {
    assert_eq!(application.kind(), "application_expression");
    let children = frontier_children(application, source, &[callee, argument]);
    assert_eq!(application.start_byte(), children[0].start_byte());
    assert_eq!(application.end_byte(), children[1].end_byte());
    [children[0], children[1]]
}

fn frontier_infix<'tree>(
    expression: Node<'tree>,
    source: &str,
    left: (&str, &str),
    operator: &str,
    right: (&str, &str),
) -> [Node<'tree>; 3] {
    assert_eq!(expression.kind(), "infix_expression");
    let children = frontier_children(expression, source, &[left, ("infix_op", operator), right]);
    assert_eq!(expression.start_byte(), children[0].start_byte());
    assert_eq!(expression.end_byte(), children[2].end_byte());
    assert!(
        children
            .iter()
            .all(|child| child.kind() != "application_expression")
    );
    [children[0], children[1], children[2]]
}

fn frontier_prefix<'tree>(
    expression: Node<'tree>,
    source: &str,
    operator: &str,
    operand: &str,
) -> Node<'tree> {
    assert_eq!(expression.kind(), "prefixed_expression");
    let children = frontier_children(
        expression,
        source,
        &[("prefix_op", operator), ("long_identifier_or_op", operand)],
    );
    assert_eq!(expression.start_byte(), children[0].start_byte());
    assert_eq!(expression.end_byte(), children[1].end_byte());
    children[1]
}

fn frontier_signed_constant(value: Node<'_>, source: &str, written: &str) {
    assert_eq!(value.kind(), "const");
    span(value, source, written);
    let children = frontier_children(value, source, &[("int", written)]);
    assert_eq!(children[0].byte_range(), value.byte_range());
    assert!(!nodes_from_owner(value).iter().any(|node| {
        matches!(
            node.kind(),
            "prefixed_expression" | "prefix_op" | "infix_op"
        )
    }));
}

fn nodes_from_owner(owner: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![owner];
    let mut found = Vec::new();
    while let Some(node) = pending.pop() {
        assert!(found.len() < 20_000);
        found.push(node);
        for index in (0..node.child_count()).rev() {
            pending.push(node.child(u32::try_from(index).unwrap()).unwrap());
        }
    }
    found
}

fn frontier_explicit_queries(tree: &Tree, source: &str) {
    let selected = [
        "application_expression",
        "infix_expression",
        "prefixed_expression",
        "ce_expression",
        "paren_expression",
        "const",
        "long_identifier_or_op",
        "prefix_op",
        "infix_op",
    ];
    let query = Query::new(
        &tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
        "[(application_expression) (infix_expression) (prefixed_expression) \
         (ce_expression) (paren_expression) (const) (long_identifier_or_op) \
         (prefix_op) (infix_op)] @syntax",
    )
    .expect("explicit frontier public-node query must compile");
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(1_024);
    let mut events = Vec::new();
    {
        let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
        while let Some((found, index)) = captures.next() {
            assert!(events.len() < 20_000);
            let capture = found.captures[*index];
            assert_eq!(
                query.capture_names()[usize::try_from(capture.index).unwrap()],
                "syntax"
            );
            events.push(capture.node);
        }
    }
    assert!(!cursor.did_exceed_match_limit());
    let walked: Vec<_> = nodes(tree)
        .into_iter()
        .map(|(node, _)| node)
        .filter(|node| selected.contains(&node.kind()))
        .collect();
    assert_eq!(events.len(), walked.len());
    // Raw events stay intact. Duplicate or absent captures fail rather than
    // being normalized into an apparently complete selector result.
    for expected in walked {
        assert_eq!(events.iter().filter(|node| **node == expected).count(), 1);
        span(expected, source, text(expected, source));
        for actual in events.iter().filter(|node| **node == expected) {
            assert_eq!(actual.parent(), expected.parent());
            assert_eq!(actual.byte_range(), expected.byte_range());
        }
    }
}

fn frontier_run_body<'tree>(tree: &'tree Tree, source: &str) -> Node<'tree> {
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let start = source.find("let run ").unwrap();
    let end = source.rfind(&format!("{newline}{FOLLOWING}")).unwrap();
    let binding = exact(tree, source, "function_or_value_defn", &source[start..end]);
    let expression = binding.child_by_field_name("body").unwrap();
    let body_start =
        source[start..end].find(&format!("{newline}    ")).unwrap() + start + newline.len() + 4;
    span(expression, source, &source[body_start..end]);
    assert_eq!(expression.parent(), Some(binding));
    let declaration = binding.parent().unwrap();
    assert_eq!(declaration.kind(), "declaration_expression");
    let module = declaration.parent().unwrap();
    assert_eq!(module.kind(), "named_module");
    let module_name = source
        .lines()
        .next()
        .unwrap()
        .strip_prefix("module ")
        .unwrap();
    span(
        module.child_by_field_name("name").unwrap(),
        source,
        module_name,
    );
    let following = exact(tree, source, "function_or_value_defn", FOLLOWING);
    let following_declaration = following.parent().unwrap();
    assert_eq!(following_declaration.kind(), "declaration_expression");
    assert_eq!(following_declaration.parent(), Some(module));
    assert_ne!(declaration, following_declaration);
    assert_eq!(
        following.child_by_field_name("body").unwrap().kind(),
        "const"
    );
    span(following.child_by_field_name("body").unwrap(), source, "30");
    assert!(declaration.end_byte() <= following_declaration.start_byte());
    assert!(expression.end_byte() < following.start_byte());
    expression
}

fn frontier_yield<'tree>(yield_expression: Node<'tree>, source: &str, operand: &str) {
    assert_eq!(yield_expression.kind(), "prefixed_expression");
    span(yield_expression, source, &format!("yield {operand}"));
    let kind = if operand == "1" {
        "const"
    } else {
        "long_identifier_or_op"
    };
    frontier_children(yield_expression, source, &[(kind, operand)]);
    let keyword = yield_expression.child(0).unwrap();
    assert_eq!(keyword.kind(), "yield");
    span(keyword, source, "yield");
    assert_eq!(keyword.parent(), Some(yield_expression));
}

fn frontier_simple_ce<'tree>(
    computation: Node<'tree>,
    source: &str,
    builder: (&str, &str),
) -> [Node<'tree>; 2] {
    assert_eq!(computation.kind(), "ce_expression");
    let children = frontier_children(
        computation,
        source,
        &[builder, ("prefixed_expression", "yield 1")],
    );
    assert_eq!(computation.start_byte(), children[0].start_byte());
    assert_eq!(
        source.get(children[0].end_byte()..children[1].start_byte()),
        Some(" { ")
    );
    assert_eq!(
        source.get(children[1].end_byte()..computation.end_byte()),
        Some(" }")
    );
    frontier_yield(children[1], source, "1");
    [children[0], children[1]]
}

fn check_frontier(index: usize) {
    let fixture = &FRONTIER_SOURCES[index];
    let source = fixture.source;
    assert!(!source.ends_with('\n'), "{}", fixture.id);
    let tree = parse(&mut parser(false), source, None);
    clean(&tree, source);
    let run_body = frontier_run_body(&tree, source);
    let expression = if source.starts_with("module ComputationFrontierExample") {
        let operands = named_operands(run_body);
        assert_eq!(run_body.kind(), "sequential_expression");
        assert_eq!(operands.len(), 4);
        for operand in &operands {
            assert_eq!(operand.parent(), Some(run_body));
        }
        span(operands[0], source, "ignore 101");
        frontier_application(
            operands[0],
            source,
            ("long_identifier_or_op", "ignore"),
            ("const", "101"),
        );
        span(operands[2], source, "ignore 103");
        frontier_application(
            operands[2],
            source,
            ("long_identifier_or_op", "ignore"),
            ("const", "103"),
        );
        assert_eq!(operands[3].kind(), "const");
        span(operands[3], source, "()");
        operands[1]
    } else {
        run_body
    };
    span(expression, source, fixture.target);
    match fixture.family {
        "adjacent-string-and-name" => {
            let [callee, _] = frontier_application(
                expression,
                source,
                ("application_expression", "printfn\"%s\""),
                ("long_identifier_or_op", "value"),
            );
            let [_, argument] = frontier_application(
                callee,
                source,
                ("long_identifier_or_op", "printfn"),
                ("const", "\"%s\""),
            );
            frontier_children(argument, source, &[("string", "\"%s\"")]);
            assert_eq!(
                callee.end_byte(),
                named_operands(expression)[1].start_byte()
            );
        }
        "adjacent-parenthesized-callee" => {
            let [callee, argument] = frontier_application(
                expression,
                source,
                ("paren_expression", "(combine)"),
                ("const", "1"),
            );
            frontier_children(callee, source, &[("long_identifier_or_op", "combine")]);
            assert_eq!(callee.end_byte(), argument.start_byte());
        }
        "adjacent-array-argument" => {
            let [callee, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                ("array_expression", "[|1|]"),
            );
            frontier_children(argument, source, &[("const", "1")]);
            assert_eq!(callee.end_byte(), argument.start_byte());
        }
        "adjacent-record-argument" => {
            let [callee, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "consume"),
                ("brace_expression", "{ Value = 1 }"),
            );
            assert_eq!(callee.end_byte(), argument.start_byte());
            let value = exact(&tree, source, "const", "1");
            assert!(
                std::iter::successors(value.parent(), |node| node.parent())
                    .any(|node| node == argument)
            );
        }
        "plus-prefix-left-space" => {
            let [_, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                ("prefixed_expression", "+value"),
            );
            frontier_prefix(argument, source, "+", "value");
        }
        "plus-infix-no-spaces" | "plus-infix-both-spaces" => {
            frontier_infix(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                "+",
                ("long_identifier_or_op", "value"),
            );
        }
        "plus-before-parenthesized-right" => {
            let [_, _, right] = frontier_infix(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                "+",
                ("paren_expression", "(value)"),
            );
            frontier_children(right, source, &[("long_identifier_or_op", "value")]);
        }
        "curry-same-line" | "curry-multiline" => {
            let children = named_operands(expression);
            assert_eq!(children.len(), 2);
            let callee_text = if fixture.family == "curry-same-line" {
                "combine 201".to_owned()
            } else {
                format!(
                    "combine{}        201",
                    if source.contains("\r\n") {
                        "\r\n"
                    } else {
                        "\n"
                    }
                )
            };
            let [callee, _] = frontier_application(
                expression,
                source,
                ("application_expression", &callee_text),
                ("const", "202"),
            );
            frontier_application(
                callee,
                source,
                ("long_identifier_or_op", "combine"),
                ("const", "201"),
            );
        }
        "infix-same-line" | "infix-continuation" => {
            frontier_infix(expression, source, ("const", "201"), "+", ("const", "202"));
        }
        "minus-adjacent-argument" | "plus-adjacent-argument" => {
            let written = if fixture.family == "minus-adjacent-argument" {
                "-1"
            } else {
                "+1"
            };
            let [_, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                ("const", written),
            );
            frontier_signed_constant(argument, source, written);
        }
        "minus-spaced-argument" | "minus-no-spaces" => {
            frontier_infix(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                "-",
                ("const", "1"),
            );
        }
        "plus-spaced-argument" | "plus-continuation" => {
            frontier_infix(
                expression,
                source,
                ("long_identifier_or_op", "combine"),
                "+",
                ("const", "1"),
            );
        }
        "bitwise-prefix-argument" | "dereference-prefix-argument" => {
            let (written, operator) = if fixture.family == "bitwise-prefix-argument" {
                ("~~~value", "~~~")
            } else {
                ("!value", "!")
            };
            let [_, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "ignore"),
                ("prefixed_expression", written),
            );
            frontier_prefix(argument, source, operator, "value");
        }
        "application-valued-builder" => {
            span(expression, source, "make value { yield 1 }");
            let [builder, _] =
                frontier_simple_ce(expression, source, ("application_expression", "make value"));
            frontier_application(
                builder,
                source,
                ("long_identifier_or_op", "make"),
                ("long_identifier_or_op", "value"),
            );
            expression_queries(&tree, source, &[expression, builder]);
        }
        "completed-computation-callee" => {
            let [callee, _] = frontier_application(
                expression,
                source,
                ("ce_expression", "builder { yield 1 }"),
                ("long_identifier_or_op", "value"),
            );
            frontier_simple_ce(callee, source, ("long_identifier_or_op", "builder"));
            expression_queries(&tree, source, &[expression, callee]);
        }
        "parenthesized-computation-argument" => {
            let [_, argument] = frontier_application(
                expression,
                source,
                ("long_identifier_or_op", "consume"),
                ("paren_expression", "(builder { yield 1 })"),
            );
            let children = frontier_children(
                argument,
                source,
                &[("ce_expression", "builder { yield 1 }")],
            );
            frontier_simple_ce(children[0], source, ("long_identifier_or_op", "builder"));
            expression_queries(&tree, source, &[expression, argument, children[0]]);
        }
        "nested-computation-argument" => {
            let newline = if source.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            let declaration_text =
                format!("let item = consume (inner {{ yield 1 }}){newline}        yield item");
            assert_eq!(expression.kind(), "ce_expression");
            let children = frontier_children(
                expression,
                source,
                &[
                    ("long_identifier_or_op", "outer"),
                    ("declaration_expression", &declaration_text),
                ],
            );
            let declaration = children[1];
            let local = exact(
                &tree,
                source,
                "function_or_value_defn",
                "let item = consume (inner { yield 1 })",
            );
            let continuation = declaration.child_by_field_name("in").unwrap();
            assert_eq!(local.parent(), Some(declaration));
            assert_eq!(continuation.parent(), Some(declaration));
            assert_eq!(named_operands(declaration), vec![local, continuation]);
            frontier_yield(continuation, source, "item");
            let local_body = local.child_by_field_name("body").unwrap();
            assert_eq!(local_body.parent(), Some(local));
            span(local_body, source, "consume (inner { yield 1 })");
            let [_, argument] = frontier_application(
                local_body,
                source,
                ("long_identifier_or_op", "consume"),
                ("paren_expression", "(inner { yield 1 })"),
            );
            let inner =
                frontier_children(argument, source, &[("ce_expression", "inner { yield 1 }")])[0];
            frontier_simple_ce(inner, source, ("long_identifier_or_op", "inner"));
            expression_queries(&tree, source, &[expression, local_body, argument, inner]);
            assert_eq!(
                nodes(&tree)
                    .iter()
                    .filter(|(node, _)| node.kind() == "ce_expression")
                    .count(),
                2
            );
        }
        family => panic!("unqualified frontier family {family}"),
    }
    frontier_explicit_queries(&tree, source);
    expression_queries(&tree, source, &[expression]);
}

include!("test-cases.rs");

// Native controls for the exact compiler-qualified comment/layout sources.
// Included after frozen frontier helpers; ownership is public CST syntax, not
// the compiler's desugared App count or semantic coverage.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TriviaOwner {
    PrefixApplication,
    Infix,
    ValueApplication,
    CommentSibling,
    PrefixSibling,
}

include!("trivia-sources.rs");

fn trivia_comments(tree: &Tree, source: &str, run: Node<'_>, expected: Option<(&str, &str)>) {
    let comments: Vec<_> = nodes(tree)
        .into_iter()
        .map(|(node, _)| node)
        .filter(|node| matches!(node.kind(), "block_comment" | "line_comment"))
        .collect();
    if let Some((kind, written)) = expected {
        assert_eq!(comments.len(), 1, "{}", tree.root_node().to_sexp());
        let comment = comments[0];
        assert_eq!(comment.kind(), kind);
        span(comment, source, written);
        assert!(comment.is_extra(), "comment was promoted to a body operand");
        assert!(comment.parent().is_some());
        assert!(run.start_byte() <= comment.start_byte() && comment.end_byte() < run.end_byte());
        assert_eq!(source.matches(written).count(), 1);
    } else {
        assert!(comments.is_empty(), "unwritten comment was published");
    }
}

fn trivia_tree_checks(
    tree: &Tree,
    source: &str,
    target: &str,
    owner: TriviaOwner,
    comment: Option<(&str, &str)>,
) {
    clean(tree, source);
    let run = frontier_run_body(tree, source);
    assert_eq!(run.kind(), "sequential_expression");
    let operands = named_operands(run);
    let operand_count = if owner == TriviaOwner::PrefixSibling {
        5
    } else {
        4
    };
    assert_eq!(operands.len(), operand_count, "{}", run.to_sexp());
    for operand in &operands {
        assert_eq!(operand.parent(), Some(run));
        assert_ne!(operand.kind(), "sequential_expression");
    }
    for pair in operands.windows(2) {
        assert!(pair[0].end_byte() < pair[1].start_byte());
    }
    assert_eq!(run.start_byte(), operands[0].start_byte());
    assert_eq!(run.end_byte(), operands.last().unwrap().end_byte());
    assert_eq!(
        nodes(tree)
            .iter()
            .filter(|(node, _)| node.kind() == "sequential_expression")
            .count(),
        1,
        "flat authored body acquired another sequence owner"
    );

    let before = operands[0];
    let after = operands[operand_count - 2];
    let unit = operands[operand_count - 1];
    span(before, source, "ignore 101");
    span(after, source, "ignore 103");
    frontier_application(
        before,
        source,
        ("long_identifier_or_op", "ignore"),
        ("const", "101"),
    );
    frontier_application(
        after,
        source,
        ("long_identifier_or_op", "ignore"),
        ("const", "103"),
    );
    assert_eq!(unit.kind(), "const");
    span(unit, source, "()");
    let following = exact(tree, source, "function_or_value_defn", FOLLOWING);
    let following_value = following.child_by_field_name("body").unwrap();
    let mut query_targets = vec![run, before, after, unit, following_value];

    let current = operands[1];
    match owner {
        TriviaOwner::PrefixApplication => {
            span(current, source, target);
            let [callee, argument] = frontier_application(
                current,
                source,
                ("long_identifier_or_op", "combine"),
                ("prefixed_expression", "-value"),
            );
            let value = frontier_prefix(argument, source, "-", "value");
            assert_eq!(argument.parent(), Some(current));
            assert!(callee.end_byte() < argument.start_byte());
            query_targets.extend([current, callee, argument, value]);
        }
        TriviaOwner::Infix => {
            span(current, source, target);
            let [left, operator, right] = frontier_infix(
                current,
                source,
                ("long_identifier_or_op", "combine"),
                "-",
                ("long_identifier_or_op", "value"),
            );
            assert_eq!(operator.parent(), Some(current));
            assert!(left.end_byte() <= operator.start_byte());
            assert!(operator.end_byte() <= right.start_byte());
            query_targets.extend([current, left, right]);
        }
        TriviaOwner::ValueApplication => {
            span(current, source, target);
            let [callee, argument] = frontier_application(
                current,
                source,
                ("long_identifier_or_op", "combine"),
                ("long_identifier_or_op", "value"),
            );
            assert!(callee.end_byte() < argument.start_byte());
            query_targets.extend([current, callee, argument]);
        }
        TriviaOwner::CommentSibling => {
            assert_eq!(current.kind(), "long_identifier_or_op");
            span(current, source, "combine");
            assert!(current.end_byte() < after.start_byte());
            let target_start = source.find(target).unwrap();
            assert_eq!(current.start_byte(), target_start);
            assert_eq!(after.end_byte(), target_start + target.len());
            query_targets.push(current);
        }
        TriviaOwner::PrefixSibling => {
            assert_eq!(current.kind(), "long_identifier_or_op");
            span(current, source, "combine");
            let prefix = operands[2];
            span(prefix, source, "-value");
            let value = frontier_prefix(prefix, source, "-", "value");
            assert_eq!(prefix.parent(), Some(run));
            assert!(current.end_byte() < prefix.start_byte());
            let target_start = source.find(target).unwrap();
            assert_eq!(current.start_byte(), target_start);
            assert_eq!(prefix.end_byte(), target_start + target.len());
            query_targets.extend([current, prefix, value]);
        }
    }
    trivia_comments(tree, source, run, comment);
    // Specific native expression nodes must retain raw capture-one. No global
    // expression inventory or compiler/native node-count equivalence is assumed.
    expression_queries(tree, source, &query_targets);
}

fn check_trivia(index: usize) {
    let fixture = &TRIVIA_SOURCES[index];
    let source = fixture.source;
    assert!(
        !source.ends_with('\n'),
        "{}: {} changed its frozen EOF form",
        fixture.family,
        fixture.id
    );
    assert_eq!(source.matches("ignore 101").count(), 1);
    assert_eq!(source.matches("ignore 103").count(), 1);
    assert_eq!(source.matches(fixture.target).count(), 1);
    let tree = parse(&mut parser(false), source, None);
    trivia_tree_checks(
        &tree,
        source,
        fixture.target,
        fixture.owner,
        fixture.comment,
    );
}

include!("trivia-test-cases.rs");

#[test]
fn trivia_gap_and_comment_edits_restore_exact_fresh_native_snapshots() {
    for fixture in [&TRIVIA_SOURCES[0], &TRIVIA_SOURCES[1]] {
        let source = fixture.source;
        let mut reused = parser(false);
        let original = parse(&mut reused, source, None);
        trivia_tree_checks(
            &original,
            source,
            fixture.target,
            fixture.owner,
            fixture.comment,
        );
        let original_snapshot = snapshot(&original, source);
        let mut before = source.to_owned();
        let mut current = original;
        for (target, owner, comment) in [
            ("combine-value", TriviaOwner::Infix, None),
            ("combine -value", TriviaOwner::PrefixApplication, None),
            ("combine - value", TriviaOwner::Infix, None),
            ("combine -value", TriviaOwner::PrefixApplication, None),
            (
                "combine(*c*)-value",
                TriviaOwner::PrefixApplication,
                Some(("block_comment", "(*c*)")),
            ),
            (
                "combine(*c*)- value",
                TriviaOwner::Infix,
                Some(("block_comment", "(*c*)")),
            ),
            (
                "combine(*c*)-value",
                TriviaOwner::PrefixApplication,
                Some(("block_comment", "(*c*)")),
            ),
            (
                "combine (*outer(*inner*)*)-value",
                TriviaOwner::PrefixApplication,
                Some(("block_comment", "(*outer(*inner*)*)")),
            ),
            (
                "combine(*c*)-value",
                TriviaOwner::PrefixApplication,
                Some(("block_comment", "(*c*)")),
            ),
        ] {
            let after = source.replacen(fixture.target, target, 1);
            assert_eq!(after.matches(target).count(), 1);
            edit(&mut current, &before, &after);
            current = parse(&mut reused, &after, Some(&current));
            let fresh = parse(&mut parser(false), &after, None);
            trivia_tree_checks(&current, &after, target, owner, comment);
            trivia_tree_checks(&fresh, &after, target, owner, comment);
            assert_eq!(snapshot(&current, &after), snapshot(&fresh, &after));
            if after == source {
                assert_eq!(snapshot(&current, &after), original_snapshot);
            }
            before = after;
        }
        assert_eq!(before, source);
        assert_eq!(snapshot(&current, source), original_snapshot);
    }
}

// Private argument-token controls bind to the unchanged frontier helper pack.
// These proposed assertions require compiler qualification and native execution.

include!("argument-token-fixtures.rs");

fn argument_token_add_application<'tree>(
    inventory: &mut Vec<Node<'tree>>,
    application: Node<'tree>,
    source: &str,
    callee: (&str, &str),
    argument: (&str, &str),
) -> [Node<'tree>; 2] {
    let children = frontier_application(application, source, callee, argument);
    inventory.extend([application, children[0], children[1]]);
    children
}

fn argument_token_operator<'tree>(
    inventory: &mut Vec<Node<'tree>>,
    application: Node<'tree>,
    source: &str,
    operator: &str,
) {
    let [callee, argument] = argument_token_add_application(
        inventory,
        application,
        source,
        ("long_identifier_or_op", "consume"),
        ("long_identifier_or_op", operator),
    );
    let leaf = frontier_children(argument, source, &[("op_identifier", operator)]);
    assert_eq!(leaf[0].byte_range(), argument.byte_range());
    let gap = &source[callee.end_byte()..argument.start_byte()];
    assert!(gap.is_empty() || gap == " ");
}

fn argument_token_check(index: usize) {
    let fixture = &ARGUMENT_TOKEN_SOURCES[index];
    let source = fixture.source;
    assert!(!source.ends_with('\n'), "{}", fixture.id);
    let tree = parse(&mut parser(false), source, None);
    clean(&tree, source);
    let owner = frontier_run_body(&tree, source);
    assert_eq!(owner.kind(), "sequential_expression");

    let mut expected = vec![("application_expression", "ignore 101")];
    for target in fixture.targets {
        let kind = if matches!(
            fixture.family,
            ArgumentTokenFamily::GenericConstructorComment
        ) {
            "generic_new_expression"
        } else {
            "application_expression"
        };
        expected.push((kind, *target));
    }
    expected.extend([("application_expression", "ignore 103"), ("const", "()")]);
    let operands = frontier_children(owner, source, &expected);
    assert_eq!(owner.start_byte(), operands[0].start_byte());
    assert_eq!(owner.end_byte(), operands.last().unwrap().end_byte());

    // This inventory is authored independently of query results. Raw unit,
    // operator-reference leaves, binding heads and record/type names stay out.
    let mut inventory = vec![owner];
    argument_token_add_application(
        &mut inventory,
        operands[0],
        source,
        ("long_identifier_or_op", "ignore"),
        ("const", "101"),
    );
    let after = operands.len() - 2;
    argument_token_add_application(
        &mut inventory,
        operands[after],
        source,
        ("long_identifier_or_op", "ignore"),
        ("const", "103"),
    );
    inventory.push(operands[after + 1]);
    frontier_children(operands[after + 1], source, &[("unit", "()")]);
    let following = exact(&tree, source, "function_or_value_defn", FOLLOWING);
    inventory.push(following.child_by_field_name("body").unwrap());

    match fixture.family {
        ArgumentTokenFamily::MultiplyPair | ArgumentTokenFamily::PlusPair => {
            let operator = if matches!(fixture.family, ArgumentTokenFamily::MultiplyPair) {
                "(*)"
            } else {
                "(+)"
            };
            argument_token_operator(&mut inventory, operands[1], source, operator);
            argument_token_operator(&mut inventory, operands[2], source, operator);
        }
        ArgumentTokenFamily::UnitAdjacent => {
            let children = frontier_children(
                operands[1],
                source,
                &[("long_identifier_or_op", "consume"), ("unit", "()")],
            );
            assert_eq!(operands[1].start_byte(), children[0].start_byte());
            assert_eq!(operands[1].end_byte(), children[1].end_byte());
            assert_eq!(children[0].end_byte(), children[1].start_byte());
            inventory.extend([operands[1], children[0]]);
        }
        ArgumentTokenFamily::UnitSpaced => {
            let [callee, argument] = argument_token_add_application(
                &mut inventory,
                operands[1],
                source,
                ("long_identifier_or_op", "consume"),
                ("const", "()"),
            );
            assert_eq!(&source[callee.end_byte()..argument.start_byte()], " ");
            frontier_children(argument, source, &[("unit", "()")]);
        }
        ArgumentTokenFamily::ParenthesisPair => {
            let immediate = operands[1];
            let children = frontier_children(
                immediate,
                source,
                &[
                    ("long_identifier_or_op", "consume"),
                    ("long_identifier_or_op", "value"),
                ],
            );
            assert_eq!(immediate.start_byte(), children[0].start_byte());
            assert_eq!(
                &source[children[0].end_byte()..children[1].start_byte()],
                "("
            );
            assert_eq!(&source[children[1].end_byte()..immediate.end_byte()], ")");
            inventory.extend([immediate, children[0], children[1]]);
            let [callee, argument] = argument_token_add_application(
                &mut inventory,
                operands[2],
                source,
                ("long_identifier_or_op", "consume"),
                ("paren_expression", "(value)"),
            );
            assert_eq!(&source[callee.end_byte()..argument.start_byte()], " ");
            let inner = frontier_children(argument, source, &[("long_identifier_or_op", "value")]);
            inventory.push(inner[0]);
        }
        ArgumentTokenFamily::GenericConstructorComment => {
            let constructor = operands[1];
            let children = frontier_children(
                constructor,
                source,
                &[("type_argument", "'T"), ("long_identifier_or_op", "value")],
            );
            assert_eq!(
                &source[constructor.start_byte()..children[0].start_byte()],
                "new "
            );
            assert_eq!(
                &source[children[0].end_byte()..children[1].start_byte()],
                "(*c*)("
            );
            assert_eq!(&source[children[1].end_byte()..constructor.end_byte()], ")");
            let comment = exact(&tree, source, "block_comment", "(*c*)");
            assert!(comment.is_extra());
            assert!(
                std::iter::successors(comment.parent(), |node| node.parent())
                    .any(|node| node == constructor)
            );
            inventory.extend([constructor, children[1]]);
        }
        ArgumentTokenFamily::AdjacentPlainString => {
            let [callee, argument] = argument_token_add_application(
                &mut inventory,
                operands[1],
                source,
                ("application_expression", "printfn\"%s\""),
                ("long_identifier_or_op", "value"),
            );
            assert_eq!(callee.end_byte(), argument.start_byte());
            let [inner_callee, inner_argument] = frontier_application(
                callee,
                source,
                ("long_identifier_or_op", "printfn"),
                ("const", "\"%s\""),
            );
            assert_eq!(inner_callee.end_byte(), inner_argument.start_byte());
            frontier_children(inner_argument, source, &[("string", "\"%s\"")]);
            inventory.extend([inner_callee, inner_argument]);
        }
        ArgumentTokenFamily::AdjacentArrayRecord => {
            let [callee, array] = argument_token_add_application(
                &mut inventory,
                operands[1],
                source,
                ("long_identifier_or_op", "combine"),
                ("array_expression", "[|1|]"),
            );
            assert_eq!(callee.end_byte(), array.start_byte());
            let array_values = frontier_children(array, source, &[("const", "1")]);
            inventory.push(array_values[0]);

            let [callee, record] = argument_token_add_application(
                &mut inventory,
                operands[2],
                source,
                ("long_identifier_or_op", "consume"),
                ("brace_expression", "{ Value = 1 }"),
            );
            assert_eq!(callee.end_byte(), record.start_byte());
            let fields = frontier_children(record, source, &[("field_initializers", "Value = 1")]);
            let entries =
                frontier_children(fields[0], source, &[("field_initializer", "Value = 1")]);
            let values = frontier_children(
                entries[0],
                source,
                &[("long_identifier", "Value"), ("const", "1")],
            );
            assert_eq!(entries[0].child_by_field_name("field"), Some(values[0]));
            assert_eq!(entries[0].child_by_field_name("value"), Some(values[1]));
            inventory.push(values[1]);
        }
    }

    let mut identities = std::collections::BTreeSet::new();
    for expression in &inventory {
        assert!(
            identities.insert(expression.id()),
            "duplicate authored expression identity"
        );
    }
    let expected_count = match fixture.family {
        ArgumentTokenFamily::MultiplyPair | ArgumentTokenFamily::PlusPair => 15,
        ArgumentTokenFamily::UnitAdjacent | ArgumentTokenFamily::GenericConstructorComment => 11,
        ArgumentTokenFamily::UnitSpaced => 12,
        ArgumentTokenFamily::ParenthesisPair => 16,
        ArgumentTokenFamily::AdjacentPlainString => 14,
        ArgumentTokenFamily::AdjacentArrayRecord => 17,
    };
    assert_eq!(inventory.len(), expected_count);
    assert_eq!(
        expression_queries(&tree, source, &inventory),
        expected_count
    );
    frontier_explicit_queries(&tree, source);
}

#[test]
fn argument_token_multiply_pair_lf_eof() {
    argument_token_check(0);
}

#[test]
fn argument_token_multiply_pair_crlf_eof() {
    argument_token_check(1);
}

#[test]
fn argument_token_plus_pair_lf_eof() {
    argument_token_check(2);
}

#[test]
fn argument_token_plus_pair_crlf_eof() {
    argument_token_check(3);
}

#[test]
fn argument_token_unit_adjacent_lf_eof() {
    argument_token_check(4);
}

#[test]
fn argument_token_unit_adjacent_crlf_eof() {
    argument_token_check(5);
}

#[test]
fn argument_token_unit_spaced_lf_eof() {
    argument_token_check(6);
}

#[test]
fn argument_token_unit_spaced_crlf_eof() {
    argument_token_check(7);
}

#[test]
fn argument_token_parenthesis_pair_lf_eof() {
    argument_token_check(8);
}

#[test]
fn argument_token_parenthesis_pair_crlf_eof() {
    argument_token_check(9);
}

#[test]
fn argument_token_generic_constructor_comment_lf_eof() {
    argument_token_check(10);
}

#[test]
fn argument_token_generic_constructor_comment_crlf_eof() {
    argument_token_check(11);
}

#[test]
fn argument_token_adjacent_plain_string_lf_eof() {
    argument_token_check(12);
}

#[test]
fn argument_token_adjacent_plain_string_crlf_eof() {
    argument_token_check(13);
}

#[test]
fn argument_token_adjacent_array_record_lf_eof() {
    argument_token_check(14);
}

#[test]
fn argument_token_adjacent_array_record_crlf_eof() {
    argument_token_check(15);
}

// Native ownership controls for six compiler-qualified specialized arguments.
// Included after the frozen frontier pack; ParseFile and native node schemas
// remain distinct, and the expression inventory is authored before execution.

include!("specialized-argument-fixtures.rs");

fn specialized_argument_punctuation(owner: Node<'_>, source: &str, index: u32, spelling: &str) {
    let punctuation = owner.child(index).unwrap();
    assert_eq!(punctuation.kind(), spelling);
    assert!(!punctuation.is_named());
    assert!(!punctuation.is_extra());
    assert_eq!(punctuation.parent(), Some(owner));
    span(punctuation, source, spelling);
}

fn specialized_argument_check(index: usize) {
    let fixture = &SPECIALIZED_ARGUMENT_SOURCES[index];
    let source = fixture.source;
    assert!(!source.ends_with('\n'), "{}", fixture.id);
    let tree = parse(&mut parser(false), source, None);
    clean(&tree, source);
    let application = frontier_run_body(&tree, source);
    let argument_contract = match fixture.family {
        SpecializedArgumentFamily::VerbatimString => ("const", "@\"neutral\""),
        SpecializedArgumentFamily::InterpolatedString => ("const", "$\"neutral {value}\""),
        SpecializedArgumentFamily::Quotation => ("literal_expression", "<@ value @>"),
    };
    let [callee, argument] = frontier_application(
        application,
        source,
        ("long_identifier_or_op", "consume"),
        argument_contract,
    );
    assert_eq!(application.child_count(), 2);
    assert_eq!(&source[callee.end_byte()..argument.start_byte()], " ");
    for child_index in 0..application.child_count() {
        assert_eq!(
            application.field_name_for_child(u32::try_from(child_index).unwrap()),
            None,
        );
    }

    let binding = application.parent().unwrap();
    assert_eq!(binding.child_by_field_name("body"), Some(application));
    let head = exact(&tree, source, "function_declaration_left", "run value");
    assert_eq!(head.parent(), Some(binding));
    let head_children = frontier_children(
        head,
        source,
        &[("identifier", "run"), ("argument_patterns", "value")],
    );
    let binder = frontier_children(head_children[1], source, &[("long_identifier", "value")]);
    assert!(binder[0].end_byte() < application.start_byte());

    // These identities are selected from the prescribed native structure,
    // never inferred from query results. Literal wrappers and binding syntax
    // are excluded by the closed raw-capture cardinality below.
    let following = exact(&tree, source, "function_or_value_defn", FOLLOWING);
    let mut inventory = vec![
        application,
        callee,
        argument,
        following.child_by_field_name("body").unwrap(),
    ];
    let expected_count = match fixture.family {
        SpecializedArgumentFamily::VerbatimString => {
            let literal =
                frontier_children(argument, source, &[("verbatim_string", "@\"neutral\"")]);
            frontier_children(literal[0], source, &[]);
            4
        }
        SpecializedArgumentFamily::InterpolatedString => {
            let string = frontier_children(argument, source, &[("string", "$\"neutral {value}\"")]);
            let format = frontier_children(
                string[0],
                source,
                &[("format_string", "$\"neutral {value}\"")],
            );
            let evaluation =
                frontier_children(format[0], source, &[("format_string_eval", "{value}")]);
            let expression =
                frontier_children(evaluation[0], source, &[("long_identifier_or_op", "value")]);
            assert_eq!(evaluation[0].child_count(), 3);
            specialized_argument_punctuation(evaluation[0], source, 0, "{");
            specialized_argument_punctuation(evaluation[0], source, 2, "}");
            assert_ne!(expression[0].id(), binder[0].id());
            assert!(expression[0].start_byte() > binder[0].end_byte());
            inventory.push(expression[0]);
            5
        }
        SpecializedArgumentFamily::Quotation => {
            let quoted = frontier_children(argument, source, &[("long_identifier_or_op", "value")]);
            assert_eq!(argument.child_count(), 3);
            specialized_argument_punctuation(argument, source, 0, "<@");
            specialized_argument_punctuation(argument, source, 2, "@>");
            assert_ne!(quoted[0].id(), binder[0].id());
            assert!(quoted[0].start_byte() > binder[0].end_byte());
            inventory.push(quoted[0]);
            5
        }
    };
    for (position, expression) in inventory.iter().enumerate() {
        assert!(
            inventory[..position]
                .iter()
                .all(|other| other.id() != expression.id())
        );
    }
    assert_eq!(inventory.len(), expected_count);
    assert_eq!(
        expression_queries(&tree, source, &inventory),
        expected_count
    );
    frontier_explicit_queries(&tree, source);
}

#[test]
fn specialized_argument_verbatim_string_lf_eof() {
    specialized_argument_check(0);
}

#[test]
fn specialized_argument_verbatim_string_crlf_eof() {
    specialized_argument_check(1);
}

#[test]
fn specialized_argument_interpolated_string_lf_eof() {
    specialized_argument_check(2);
}

#[test]
fn specialized_argument_interpolated_string_crlf_eof() {
    specialized_argument_check(3);
}

#[test]
fn specialized_argument_quotation_lf_eof() {
    specialized_argument_check(4);
}

#[test]
fn specialized_argument_quotation_crlf_eof() {
    specialized_argument_check(5);
}

// Source ownership controls retain the unfielded method-expression body contract.
// The earlier prospective block-field expectation was invalid for this route.
include!("object-owner-sources-v2.rs");

fn object_owner_explicit_queries(tree: &Tree, source: &str, expected: &[Node<'_>]) {
    let mut kinds: Vec<_> = expected.iter().map(|node| node.kind()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    let pattern = kinds
        .iter()
        .map(|kind| format!("({kind}) @owner\n"))
        .collect::<String>();
    let query = Query::new(&tree_sitter_fsharp::LANGUAGE_FSHARP.into(), &pattern)
        .expect("prospective explicit owner query must compile");
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(1_024);
    let mut events = Vec::new();
    {
        let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
        while let Some((found, index)) = captures.next() {
            assert!(events.len() < 20_000);
            events.push(found.captures[*index].node);
        }
    }
    assert!(!cursor.did_exceed_match_limit());
    let walked: Vec<_> = nodes(tree)
        .into_iter()
        .map(|(node, _)| node)
        .filter(|node| kinds.contains(&node.kind()))
        .collect();
    assert_eq!(events.len(), walked.len());
    for node in walked {
        assert_eq!(events.iter().filter(|actual| **actual == node).count(), 1);
        for actual in events.iter().filter(|actual| **actual == node) {
            assert_eq!(actual.parent(), node.parent());
            assert_eq!(actual.byte_range(), node.byte_range());
            span(*actual, source, text(node, source));
        }
    }
    for node in expected {
        assert_eq!(events.iter().filter(|actual| **actual == *node).count(), 1);
    }
}

fn proposed_object_owner(index: usize) {
    let fixture = &OBJECT_OWNER_SOURCES[index];
    let source = fixture.source;
    assert!(!source.ends_with('\n'), "{}", fixture.id);
    let tree = parse(&mut parser(false), source, None);
    if fixture.route == "recovery" {
        // The missing member RHS is rejected by the separately qualified ParseFile oracle.
        assert!(
            tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert!(
            nodes(&tree)
                .iter()
                .any(|(node, _)| node.is_error() || node.is_missing())
        );
        assert_eq!(tree.root_node().end_byte(), source.len());
        // Recovery placement and later-owner retention are not yet qualified.
        return;
    }
    clean(&tree, source);
    let brace = frontier_run_body(&tree, source);
    assert_eq!(brace.kind(), "brace_expression");
    span(brace, source, fixture.target);
    let owner = brace.child_by_field_name("block").unwrap();
    assert_eq!(owner.parent(), Some(brace));
    span(owner, source, fixture.owner);
    if fixture.route == "object" {
        assert_eq!(owner.kind(), "object_expression");
        let member = exact(&tree, source, "member_defn", "member this.Read() = 101");
        assert_eq!(member.parent(), Some(owner));
        assert_eq!(owner.child_by_field_name("block"), Some(member));
        let constructor = exact(&tree, source, fixture.constructor_kind, fixture.constructor);
        assert_eq!(constructor.parent(), Some(owner));
        assert_eq!(named_operands(owner), vec![constructor, member]);
        let method = exact(&tree, source, "method_or_prop_defn", "this.Read() = 101");
        assert_eq!(method.parent(), Some(member));
        let name = method.child_by_field_name("name").unwrap();
        assert_eq!(name.kind(), "property_or_ident");
        span(name, source, "this.Read");
        assert_eq!(name.parent(), Some(method));
        let body = exact(&tree, source, "const", "101");
        assert_eq!(body.parent(), Some(method));
        // _method_defn uses an unfielded _expression_block; other method/property
        // alternatives in node-types.json do not confer their fields on this route.
        assert_eq!(method.child_by_field_name("block"), None);
        assert_eq!(method.child_by_field_name("body"), None);
        let method_children = frontier_children(
            method,
            source,
            &[
                ("property_or_ident", "this.Read"),
                ("const", "()"),
                ("const", "101"),
            ],
        );
        assert_eq!(method_children[0], name);
        assert_eq!(method_children[2], body);
        assert_eq!(method.child_by_field_name("args"), Some(method_children[1]));
        if fixture.constructor == "Receiver()" {
            frontier_children(
                constructor,
                source,
                &[("long_identifier_or_op", "Receiver"), ("unit", "()")],
            );
        } else if fixture.constructor == "Receiver(1)" {
            frontier_children(
                constructor,
                source,
                &[("long_identifier_or_op", "Receiver"), ("const", "1")],
            );
        }
        assert!(
            !nodes_from_owner(owner)
                .iter()
                .any(|node| matches!(node.kind(), "with_field_expression" | "field_initializer"))
        );
        object_owner_explicit_queries(
            &tree,
            source,
            &[brace, owner, constructor, member, method, body],
        );
        expression_queries(&tree, source, &[brace, constructor, body]);
    } else {
        assert_eq!(fixture.route, "copy");
        assert_eq!(owner.kind(), "with_field_expression");
        let base = exact(&tree, source, fixture.base_kind, fixture.base);
        assert_eq!(base.parent(), Some(owner));
        let fields = exact(&tree, source, "field_initializers", "Value = 101");
        assert_eq!(fields.parent(), Some(owner));
        assert_eq!(owner.child_by_field_name("block"), Some(fields));
        assert_eq!(named_operands(owner), vec![base, fields]);
        let field = exact(&tree, source, "field_initializer", "Value = 101");
        assert_eq!(field.parent(), Some(fields));
        let field_name = field.child_by_field_name("field").unwrap();
        assert_eq!(field_name.kind(), "long_identifier");
        span(field_name, source, "Value");
        assert_eq!(field_name.parent(), Some(field));
        let value = field.child_by_field_name("value").unwrap();
        assert_eq!(value.kind(), "const");
        span(value, source, "101");
        assert_eq!(value.parent(), Some(field));
        let mut expressions = vec![brace, base, value];
        let mut public_owners = vec![brace, owner, base, fields, field, value];
        if fixture.base_kind == "paren_expression" {
            let prefix = exact(&tree, source, "prefixed_expression", "new Receiver()");
            assert_eq!(prefix.parent(), Some(base));
            let constructor = exact(&tree, source, "application_expression", "Receiver()");
            assert_eq!(constructor.parent(), Some(prefix));
            expressions.extend([prefix, constructor]);
            public_owners.extend([prefix, constructor]);
        }
        assert!(
            !nodes_from_owner(owner)
                .iter()
                .any(|node| matches!(node.kind(), "object_expression" | "member_defn"))
        );
        object_owner_explicit_queries(&tree, source, &public_owners);
        expression_queries(&tree, source, &expressions);
    }
}

#[test]
fn proposed_interface_object_keeps_member_owner() {
    for index in [0, 1] {
        proposed_object_owner(index);
    }
}

#[test]
fn proposed_unit_constructor_object_keeps_member_owner() {
    for index in [2, 3] {
        proposed_object_owner(index);
    }
}

#[test]
fn proposed_literal_constructor_object_keeps_member_owner() {
    for index in [4, 5] {
        proposed_object_owner(index);
    }
}

#[test]
fn proposed_identifier_record_copy_keeps_field_owner() {
    for index in [6, 7] {
        proposed_object_owner(index);
    }
}

#[test]
fn proposed_parenthesized_constructor_record_copy_keeps_field_owner() {
    for index in [8, 9] {
        proposed_object_owner(index);
    }
}

#[test]
fn proposed_object_member_without_rhs_requires_recovery() {
    for index in [10, 11] {
        proposed_object_owner(index);
    }
}

include!("boundary-controls.rs");
include!("quote-controls.rs");

include!("do-terminator-controls.rs");
include!("module-do-controls.rs");

include!("eof-controls.rs");
include!("eof-incremental-controls.rs");

include!("callee-controls.rs");

include!("shared-consumer-controls.rs");

include!("nested-qualified-controls.rs");
include!("inline-value-name-controls.rs");
