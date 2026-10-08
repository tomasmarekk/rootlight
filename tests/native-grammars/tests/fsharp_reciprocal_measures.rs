//! Neutral measure-expression controls pin written reciprocal operands in both profiles.
//! These syntax-only tests use the native harness bounds, not F# name resolution or typing.
//! A leading slash owns a denominator without synthesizing a written numerator.

use std::{
    ops::{ControlFlow, Range},
    time::{Duration, Instant},
};
use tree_sitter::{
    InputEdit, Language, Node, ParseOptions, Parser, Point, Query, QueryCursor, QueryCursorOptions,
    StreamingIterator, Tree,
};

type Operand = (&'static str, &'static str);

fn language(signature: bool) -> Language {
    if signature {
        tree_sitter_fsharp::LANGUAGE_SIGNATURE
    } else {
        tree_sitter_fsharp::LANGUAGE_FSHARP
    }
    .into()
}

fn parser(signature: bool) -> Parser {
    let mut parser = Parser::new();
    parser.set_language(&language(signature)).unwrap();
    parser
}

fn parse(parser: &mut Parser, source: &str, previous: Option<&Tree>) -> Tree {
    assert!(source.len() < 4_096, "authored source exceeds its bound");
    let started = Instant::now();
    let mut checks = 0_usize;
    let mut progress = |_: &tree_sitter::ParseState| {
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
            previous,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        )
        .expect("authored measure exceeded its parse watchdog")
}

fn nodes(tree: &Tree) -> Vec<Node<'_>> {
    let mut pending = vec![tree.root_node()];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        assert!(result.len() < 10_000, "node inventory exceeds its bound");
        result.push(node);
        for index in (0..node.child_count()).rev() {
            pending.push(node.child(u32::try_from(index).unwrap()).unwrap());
        }
    }
    result
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    (0..node.named_child_count())
        .map(|index| node.named_child(u32::try_from(index).unwrap()).unwrap())
        .filter(|child| !child.is_extra())
        .collect()
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

fn written(node: Node<'_>, source: &str, kind: &str, value: &str) {
    assert_eq!(node.kind(), kind);
    assert_eq!(source.get(node.byte_range()), Some(value));
    assert_eq!(node.end_byte() - node.start_byte(), value.len());
    assert_eq!(node.start_position(), point(source, node.start_byte()));
    assert_eq!(node.end_position(), point(source, node.end_byte()));
}

fn occurrence(source: &str, value: &str, ordinal: usize) -> Range<usize> {
    let start = source.match_indices(value).nth(ordinal).unwrap().0;
    start..start + value.len()
}

fn exact<'tree>(tree: &'tree Tree, kind: &str, range: Range<usize>) -> Node<'tree> {
    let found: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == kind && node.byte_range() == range)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{kind} {range:?}: {}",
        tree.root_node().to_sexp()
    );
    found[0]
}

fn fixture(signature: bool, declaration: &str, ending: &str, final_newline: bool) -> String {
    let sentinel = if signature {
        "val sentinel: int"
    } else {
        "let sentinel = 73"
    };
    let source =
        format!("module ReciprocalExample\n{declaration}\n{sentinel}").replace('\n', ending);
    if final_newline {
        format!("{source}{ending}")
    } else {
        source
    }
}

#[derive(Debug, PartialEq, Eq)]
struct VisibleNode {
    kind: String,
    flags: [bool; 4],
    bytes: (usize, usize),
    points: (Point, Point),
    fields: Vec<Option<String>>,
    child_count: usize,
}

fn snapshot(tree: &Tree, source: &str) -> Vec<VisibleNode> {
    let root = tree.root_node();
    written(root, source, "file", source);
    nodes(tree)
        .into_iter()
        .map(|node| {
            assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
            assert_eq!(node.start_position(), point(source, node.start_byte()));
            assert_eq!(node.end_position(), point(source, node.end_byte()));
            if let Some(parent) = node.parent() {
                assert!(
                    parent.start_byte() <= node.start_byte()
                        && node.end_byte() <= parent.end_byte()
                );
            }
            VisibleNode {
                kind: node.kind().to_owned(),
                flags: [
                    node.is_named(),
                    node.is_extra(),
                    node.is_error(),
                    node.is_missing(),
                ],
                bytes: (node.start_byte(), node.end_byte()),
                points: (node.start_position(), node.end_position()),
                fields: (0..node.child_count())
                    .map(|index| {
                        node.field_name_for_child(u32::try_from(index).unwrap())
                            .map(str::to_owned)
                    })
                    .collect(),
                child_count: node.child_count(),
            }
        })
        .collect()
}

fn clean(tree: &Tree, source: &str) {
    assert!(
        !tree.root_node().has_error(),
        "{source:?}: {}",
        tree.root_node().to_sexp()
    );
    for node in nodes(tree) {
        assert!(!node.is_error() && !node.is_missing());
        assert!(node.start_byte() < node.end_byte());
    }
    snapshot(tree, source);
}

fn query_captures<'tree>(
    tree: &'tree Tree,
    source: &str,
    signature: bool,
    kind: &str,
) -> Vec<Node<'tree>> {
    let query = Query::new(&language(signature), &format!("({kind}) @written")).unwrap();
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(4_096);
    let started = Instant::now();
    let mut checks = 0_usize;
    let mut exhausted = false;
    let mut progress = |_: &tree_sitter::QueryCursorState| {
        checks += 1;
        if checks > 2_000 || started.elapsed() > Duration::from_secs(10) {
            exhausted = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut result = Vec::new();
    {
        let mut captures = cursor.captures_with_options(
            &query,
            tree.root_node(),
            source.as_bytes(),
            QueryCursorOptions::new().progress_callback(&mut progress),
        );
        while let Some((matched, index)) = captures.next() {
            assert!(result.len() < 4_096, "capture inventory exceeds its bound");
            let capture = matched.captures.get(*index).unwrap();
            assert_eq!(capture.index, 0);
            result.push(capture.node);
        }
    }
    assert!(!exhausted && !cursor.did_exceed_match_limit());
    result.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind()));
    result
}

fn queries(tree: &Tree, source: &str, signature: bool) {
    for kind in [
        "measure_quotient",
        "measure_product",
        "measure_power",
        "measure_atom",
    ] {
        let mut expected: Vec<_> = nodes(tree)
            .into_iter()
            .filter(|node| node.kind() == kind)
            .collect();
        expected.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind()));
        assert_eq!(query_captures(tree, source, signature, kind), expected);
    }
}

fn quotient<'tree>(
    tree: &'tree Tree,
    source: &str,
    value: &str,
    ordinal: usize,
    operands: &[Operand],
    reciprocal: bool,
) -> Node<'tree> {
    let owner = exact(tree, "measure_quotient", occurrence(source, value, ordinal));
    written(owner, source, "measure_quotient", value);
    let parts = children(owner);
    assert_eq!(parts.len(), operands.len());
    let mut offset = owner.start_byte();
    for (part, &(kind, text)) in parts.iter().zip(operands) {
        written(*part, source, kind, text);
        assert_eq!(part.parent(), Some(owner));
        let relative = source
            .get(offset..owner.end_byte())
            .unwrap()
            .find(text)
            .unwrap();
        assert_eq!(part.start_byte(), offset + relative);
        offset = part.end_byte();
    }
    assert_eq!(parts.last().unwrap().end_byte(), owner.end_byte());
    let slashes: Vec<_> = (0..owner.child_count())
        .map(|index| owner.child(u32::try_from(index).unwrap()).unwrap())
        .filter(|child| child.kind() == "/")
        .collect();
    assert_eq!(
        slashes.len(),
        if reciprocal {
            operands.len()
        } else {
            operands.len() - 1
        }
    );
    for slash in &slashes {
        written(*slash, source, "/", "/");
        assert!(!slash.is_named());
        assert_eq!(slash.parent(), Some(owner));
    }
    for index in 0..owner.child_count() {
        assert!(
            owner
                .field_name_for_child(u32::try_from(index).unwrap())
                .is_none()
        );
    }
    if reciprocal {
        assert_eq!(slashes[0].start_byte(), owner.start_byte());
        assert!(slashes[0].end_byte() <= parts[0].start_byte());
        assert!(
            nodes(tree)
                .into_iter()
                .filter(|node| {
                    owner.start_byte() <= node.start_byte() && node.end_byte() <= owner.end_byte()
                })
                .all(|node| !node.is_missing()
                    && !(node.kind() == "measure_atom"
                        && source.get(node.byte_range()) == Some("1")))
        );
    } else {
        assert_eq!(parts[0].start_byte(), owner.start_byte());
    }
    owner
}

fn alias_owner(tree: &Tree, source: &str, value: &str, owner: Node<'_>) {
    let declaration = format!("rate = {value}");
    let alias = exact(
        tree,
        "type_abbrev_defn",
        occurrence(source, &declaration, 0),
    );
    let block = alias.child_by_field_name("block").unwrap();
    assert_eq!(block, measure_owner(owner, source, value));
    assert_eq!(block.parent(), Some(alias));
}

fn measure_owner<'tree>(owner: Node<'tree>, source: &str, value: &str) -> Node<'tree> {
    let measure = owner.parent().unwrap();
    written(measure, source, "measure", value);
    assert_eq!(children(measure), [owner]);
    for index in 0..measure.child_count() {
        assert!(
            measure
                .field_name_for_child(u32::try_from(index).unwrap())
                .is_none()
        );
    }
    measure
}

fn sentinel(tree: &Tree, source: &str, signature: bool) {
    let (kind, text) = if signature {
        ("value_definition", "val sentinel: int")
    } else {
        ("function_or_value_defn", "let sentinel = 73")
    };
    let owner = exact(tree, kind, occurrence(source, text, 0));
    written(owner, source, kind, text);
    assert!(!owner.has_error());
    if !signature {
        let body = owner.child_by_field_name("body").unwrap();
        written(body, source, "const", "73");
        assert_eq!(body.parent(), Some(owner));
    }
    for node in nodes(tree) {
        if node.start_byte() >= owner.start_byte() {
            assert!(!node.is_error() && !node.is_missing());
        }
    }
}

fn edit(tree: &mut Tree, before: &str, after: &str) {
    assert!(before.is_ascii() && after.is_ascii());
    let start = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = before
        .as_bytes()
        .get(start..)
        .unwrap()
        .iter()
        .rev()
        .zip(after.as_bytes().get(start..).unwrap().iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_end = before.len() - suffix;
    let new_end = after.len() - suffix;
    tree.edit(&InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point(before, start),
        old_end_position: point(before, old_end),
        new_end_position: point(after, new_end),
    });
    assert!(tree.root_node().has_changes());
}

#[test]
fn reciprocal_aliases_have_only_written_denominators_in_both_profiles() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for value in ["/ duration", "/duration"] {
                    let source = fixture(
                        signature,
                        &format!("[<Measure>]\ntype rate = {value}"),
                        ending,
                        final_newline,
                    );
                    let tree = parse(&mut parser(signature), &source, None);
                    clean(&tree, &source);
                    let owner = quotient(
                        &tree,
                        &source,
                        value,
                        0,
                        &[("measure_atom", "duration")],
                        true,
                    );
                    alias_owner(&tree, &source, value, owner);
                    sentinel(&tree, &source, signature);
                    queries(&tree, &source, signature);
                }
            }
        }
    }
}

#[test]
fn reciprocal_generic_arguments_retain_their_attribute_owner() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            let source = fixture(
                signature,
                "type Reading = float</duration>\ntype SecondReading = float</duration>",
                ending,
                true,
            );
            let tree = parse(&mut parser(signature), &source, None);
            clean(&tree, &source);
            for ordinal in [0, 1] {
                let owner = quotient(
                    &tree,
                    &source,
                    "/duration",
                    ordinal,
                    &[("measure_atom", "duration")],
                    true,
                );
                let measure = measure_owner(owner, &source, "/duration");
                let attribute = measure.parent().unwrap();
                written(attribute, &source, "type_attribute", "/duration");
                assert_eq!(children(attribute), [measure]);
                let attributes = attribute.parent().unwrap();
                written(attributes, &source, "type_attributes", "/duration");
                assert_eq!(children(attributes), [attribute]);
                let generic = attributes.parent().unwrap();
                written(generic, &source, "generic_type", "float</duration>");
                assert_eq!(children(generic).last(), Some(&attributes));
            }
            assert_eq!(
                query_captures(&tree, &source, signature, "measure_quotient").len(),
                2
            );
            sentinel(&tree, &source, signature);
            queries(&tree, &source, signature);
        }
    }
}

#[test]
fn reciprocal_numeric_suffixes_keep_the_constant_outside_the_measure() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            let source = fixture(
                signature,
                "[<Witness(17.25</duration>)>]\ntype Reading = int",
                ending,
                true,
            );
            let tree = parse(&mut parser(signature), &source, None);
            clean(&tree, &source);
            let typed = exact(
                &tree,
                "typed_expression",
                occurrence(&source, "17.25</duration>", 0),
            );
            let owner = quotient(
                &tree,
                &source,
                "/duration",
                0,
                &[("measure_atom", "duration")],
                true,
            );
            let measure = measure_owner(owner, &source, "/duration");
            let constant = exact(&tree, "const", occurrence(&source, "17.25", 0));
            assert_eq!(children(typed), [constant, measure]);
            assert_eq!(constant.parent(), Some(typed));
            assert_eq!(measure.parent(), Some(typed));
            written(constant, &source, "const", "17.25");
            written(children(constant)[0], &source, "float", "17.25");
            assert!(constant.end_byte() < owner.start_byte());
            sentinel(&tree, &source, signature);
            queries(&tree, &source, signature);
        }
    }
    let source = fixture(false, "let sample = 17.25</duration>", "\n", true);
    let tree = parse(&mut parser(false), &source, None);
    clean(&tree, &source);
    let typed = exact(
        &tree,
        "typed_expression",
        occurrence(&source, "17.25</duration>", 0),
    );
    let owner = quotient(
        &tree,
        &source,
        "/duration",
        0,
        &[("measure_atom", "duration")],
        true,
    );
    assert_eq!(
        measure_owner(owner, &source, "/duration").parent(),
        Some(typed)
    );
    queries(&tree, &source, false);
}

#[test]
fn reciprocal_comments_parentheses_and_powers_keep_written_boundaries() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for (value, operand) in [
                (
                    "/ (* unit boundary *) duration",
                    ("measure_atom", "duration"),
                ),
                ("/ (duration^2)", ("measure_atom", "(duration^2)")),
                ("/ duration^-2", ("measure_power", "duration^-2")),
                ("/ duration^(1/2)", ("measure_power", "duration^(1/2)")),
                ("/ duration^(-1/2)", ("measure_power", "duration^(-1/2)")),
            ] {
                let source = fixture(
                    signature,
                    &format!("[<Measure>]\ntype rate = {value}"),
                    ending,
                    true,
                );
                let tree = parse(&mut parser(signature), &source, None);
                clean(&tree, &source);
                let owner = quotient(&tree, &source, value, 0, &[operand], true);
                alias_owner(&tree, &source, value, owner);
                if value.contains("(*") {
                    let comment = exact(
                        &tree,
                        "block_comment",
                        occurrence(&source, "(* unit boundary *)", 0),
                    );
                    assert!(comment.is_extra());
                    assert!(
                        owner.start_byte() < comment.start_byte()
                            && comment.end_byte() < children(owner)[0].start_byte()
                    );
                }
                queries(&tree, &source, signature);
                sentinel(&tree, &source, signature);
            }
        }
    }
}

#[test]
fn reciprocal_type_variables_and_products_remain_direct_operands() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for (value, operand) in [
                ("/ 'u", ("measure_atom", "'u")),
                ("/ 'u^2", ("measure_power", "'u^2")),
                (
                    "/ duration^2 distance^-1",
                    ("measure_product", "duration^2 distance^-1"),
                ),
                (
                    "/ (duration / scale)",
                    ("measure_atom", "(duration / scale)"),
                ),
            ] {
                let source = fixture(
                    signature,
                    &format!("[<Measure>]\ntype rate = {value}"),
                    ending,
                    true,
                );
                let tree = parse(&mut parser(signature), &source, None);
                clean(&tree, &source);
                quotient(&tree, &source, value, 0, &[operand], true);
                if value.contains("'u") {
                    let argument = exact(&tree, "type_argument", occurrence(&source, "'u", 0));
                    written(argument, &source, "type_argument", "'u");
                    let atom = argument.parent().unwrap();
                    written(atom, &source, "measure_atom", "'u");
                    assert_eq!(children(atom), [argument]);
                }
                queries(&tree, &source, signature);
            }
        }
    }
}

#[test]
fn binary_and_reciprocal_quotient_chains_preserve_ordered_written_operands() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for (value, reciprocal, operands) in [
                (
                    "distance / duration",
                    false,
                    &[("measure_atom", "distance"), ("measure_atom", "duration")][..],
                ),
                (
                    "distance / duration / scale",
                    false,
                    &[
                        ("measure_atom", "distance"),
                        ("measure_atom", "duration"),
                        ("measure_atom", "scale"),
                    ][..],
                ),
                (
                    "/ duration / scale",
                    true,
                    &[("measure_atom", "duration"), ("measure_atom", "scale")][..],
                ),
            ] {
                let source = fixture(
                    signature,
                    &format!("[<Measure>]\ntype rate = {value}"),
                    ending,
                    true,
                );
                let tree = parse(&mut parser(signature), &source, None);
                clean(&tree, &source);
                let owner = quotient(&tree, &source, value, 0, operands, reciprocal);
                alias_owner(&tree, &source, value, owner);
                assert_eq!(
                    query_captures(&tree, &source, signature, "measure_quotient"),
                    [owner]
                );
                queries(&tree, &source, signature);
            }
        }
    }
}

#[test]
fn ordinary_products_and_explicit_dimensionless_numerators_remain_distinct() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            let source = fixture(
                signature,
                "[<Measure>]\ntype rate = distance^2 duration^-1",
                ending,
                true,
            );
            let tree = parse(&mut parser(signature), &source, None);
            clean(&tree, &source);
            let product = exact(
                &tree,
                "measure_product",
                occurrence(&source, "distance^2 duration^-1", 0),
            );
            let operands = children(product);
            assert_eq!(operands.len(), 2);
            for (part, text) in operands.iter().zip(["distance^2", "duration^-1"]) {
                written(*part, &source, "measure_power", text);
                assert_eq!(part.parent(), Some(product));
            }
            assert!(query_captures(&tree, &source, signature, "measure_quotient").is_empty());
            queries(&tree, &source, signature);
            let source = fixture(
                signature,
                "[<Measure>]\ntype rate = distance * scale / duration",
                ending,
                true,
            );
            let tree = parse(&mut parser(signature), &source, None);
            clean(&tree, &source);
            quotient(
                &tree,
                &source,
                "distance * scale / duration",
                0,
                &[
                    ("compound_type", "distance * scale"),
                    ("measure_atom", "duration"),
                ],
                false,
            );
            queries(&tree, &source, signature);
            let source = fixture(
                signature,
                "[<Measure>]\ntype rate = 1 / duration",
                ending,
                true,
            );
            let tree = parse(&mut parser(signature), &source, None);
            clean(&tree, &source);
            quotient(
                &tree,
                &source,
                "1 / duration",
                0,
                &[("measure_atom", "1"), ("measure_atom", "duration")],
                false,
            );
            queries(&tree, &source, signature);
        }
    }
}

#[test]
fn malformed_reciprocals_recover_without_consuming_the_following_declaration() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for value in ["/", "/ / duration", "/ duration /", "/ )"] {
                let source = fixture(
                    signature,
                    &format!("type Broken = float<{value}>"),
                    ending,
                    true,
                );
                let tree = parse(&mut parser(signature), &source, None);
                assert!(
                    tree.root_node().has_error(),
                    "{source:?}: {}",
                    tree.root_node().to_sexp()
                );
                assert!(
                    nodes(&tree)
                        .iter()
                        .any(|node| node.is_error() || node.is_missing())
                );
                snapshot(&tree, &source);
                sentinel(&tree, &source, signature);
                queries(&tree, &source, signature);
            }
        }
    }
}

#[test]
fn reciprocal_edits_and_malformed_restoration_match_fresh_visible_trees() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            let original = fixture(
                signature,
                "[<Measure>]\ntype rate = / duration",
                ending,
                true,
            );
            let mut shared = parser(signature);
            let mut tree = parse(&mut shared, &original, None);
            clean(&tree, &original);
            quotient(
                &tree,
                &original,
                "/ duration",
                0,
                &[("measure_atom", "duration")],
                true,
            );
            let original_snapshot = snapshot(&tree, &original);
            let mut before = original.clone();
            for value in [
                "distance / duration",
                "/ duration^2",
                "/ duration /",
                "/ duration",
            ] {
                let after = fixture(
                    signature,
                    &format!("[<Measure>]\ntype rate = {value}"),
                    ending,
                    true,
                );
                edit(&mut tree, &before, &after);
                let incremental = parse(&mut shared, &after, Some(&tree));
                let fresh = parse(&mut parser(signature), &after, None);
                assert_eq!(snapshot(&incremental, &after), snapshot(&fresh, &after));
                sentinel(&incremental, &after, signature);
                queries(&incremental, &after, signature);
                if value == "/ duration /" {
                    assert!(incremental.root_node().has_error());
                } else {
                    clean(&incremental, &after);
                    let operands: &[Operand] = match value {
                        "distance / duration" => {
                            &[("measure_atom", "distance"), ("measure_atom", "duration")]
                        }
                        "/ duration^2" => &[("measure_power", "duration^2")],
                        "/ duration" => &[("measure_atom", "duration")],
                        _ => unreachable!("closed authored sequence"),
                    };
                    quotient(
                        &incremental,
                        &after,
                        value,
                        0,
                        operands,
                        value.starts_with('/'),
                    );
                }
                tree = incremental;
                before = after;
            }
            assert_eq!(before, original);
            assert_eq!(snapshot(&tree, &before), original_snapshot);
        }
    }
}

include!("common/measure_keyword_controls.rs");
