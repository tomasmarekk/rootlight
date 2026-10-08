//! Numeric literal continuations retain written owners instead of becoming application links.
//! Syntax-only controls cover both profiles; source ranges and edits do not evaluate values.

use std::{
    ops::ControlFlow,
    time::{Duration, Instant},
};

use tree_sitter::{InputEdit, Node, ParseOptions, Parser, Point, Tree};

type LiteralComponent = (&'static str, &'static str);
type LiteralCase = (&'static str, &'static str, Option<LiteralComponent>);

const LITERALS: &[LiteralCase] = &[
    ("1", "int", None),
    ("0x1", "xint", None),
    ("-1y", "sbyte", Some(("int", "-1"))),
    ("0x1y", "sbyte", Some(("xint", "0x1"))),
    ("1uy", "byte", Some(("int", "1"))),
    ("0x1uy", "byte", Some(("xint", "0x1"))),
    ("1s", "int16", Some(("int", "1"))),
    ("0x1s", "int16", Some(("xint", "0x1"))),
    ("1us", "uint16", Some(("int", "1"))),
    ("0x1us", "uint16", Some(("xint", "0x1"))),
    ("1l", "int32", Some(("int", "1"))),
    ("0x1l", "int32", Some(("xint", "0x1"))),
    ("1u", "uint32", Some(("int", "1"))),
    ("1ul", "uint32", Some(("int", "1"))),
    ("0x1u", "uint32", Some(("xint", "0x1"))),
    ("0x1ul", "uint32", Some(("xint", "0x1"))),
    ("1n", "nativeint", Some(("int", "1"))),
    ("0x1n", "nativeint", Some(("xint", "0x1"))),
    ("1un", "unativeint", Some(("int", "1"))),
    ("0x1un", "unativeint", Some(("xint", "0x1"))),
    ("1L", "int64", Some(("int", "1"))),
    ("0x1L", "int64", Some(("xint", "0x1"))),
    ("1UL", "uint64", Some(("int", "1"))),
    ("1uL", "uint64", Some(("int", "1"))),
    ("0x1UL", "uint64", Some(("xint", "0x1"))),
    ("0x1uL", "uint64", Some(("xint", "0x1"))),
    ("1.f", "ieee32", Some(("float", "1."))),
    ("1.0F", "ieee32", Some(("float", "1.0"))),
    ("0x1lf", "ieee32", Some(("xint", "0x1"))),
    ("0x1LF", "ieee64", Some(("xint", "0x1"))),
    ("1Q", "bignum", Some(("int", "1"))),
    ("1R", "bignum", Some(("int", "1"))),
    ("1Z", "bignum", Some(("int", "1"))),
    ("1I", "bignum", Some(("int", "1"))),
    ("1N", "bignum", Some(("int", "1"))),
    ("1G", "bignum", Some(("int", "1"))),
    ("1M", "decimal", Some(("int", "1"))),
    ("1m", "decimal", Some(("int", "1"))),
    ("1.0M", "decimal", Some(("float", "1.0"))),
    ("1.0m", "decimal", Some(("float", "1.0"))),
    ("1.", "float", None),
    ("1.0", "float", None),
    ("1e2", "float", None),
    ("1.e2", "float", None),
    ("1.0e+2", "float", None),
    ("1.0E-2", "float", None),
    ("1.2_3e+4_5", "float", None),
];

fn parser(signature: bool) -> Parser {
    let mut parser = Parser::new();
    let language = if signature {
        tree_sitter_fsharp::LANGUAGE_SIGNATURE
    } else {
        tree_sitter_fsharp::LANGUAGE_FSHARP
    };
    parser.set_language(&language.into()).unwrap();
    parser
}

fn parse(parser: &mut Parser, source: &str, old: Option<&Tree>) -> Tree {
    assert!(source.len() <= 16 * 1024);
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
        .expect("numeric syntax control exceeded its parse watchdog")
}

fn nodes(tree: &Tree) -> Vec<Node<'_>> {
    let mut pending = vec![tree.root_node()];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        result.push(node);
        for index in (0..node.child_count()).rev() {
            pending.push(node.child(u32::try_from(index).unwrap()).unwrap());
        }
    }
    result
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap()
}

fn exact<'a>(tree: &'a Tree, source: &str, kind: &str, value: &str) -> Node<'a> {
    let matches: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == kind && text(*node, source) == value)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "{kind}/{value}: {}",
        tree.root_node().to_sexp()
    );
    matches[0]
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
    assert_eq!(text(node, source), value);
    assert_eq!(node.end_byte() - node.start_byte(), value.len());
    assert_eq!(node.start_position(), point(source, node.start_byte()));
    assert_eq!(node.end_position(), point(source, node.end_byte()));
}

fn fixture(signature: bool, rhs: &str, ending: &str, final_newline: bool) -> String {
    let following = if signature {
        "val following: int"
    } else {
        "let following = 30"
    };
    let source = format!(
        "module NumericLiteralExample\ntype Number =\n    | First = {rhs}\n    | Next = 2\n{following}"
    );
    let source = source.replace('\n', ending);
    if final_newline {
        format!("{source}{ending}")
    } else {
        source
    }
}

fn literal_owner(tree: &Tree, source: &str, literal: &str, kind: &str, base: Option<(&str, &str)>) {
    assert!(
        !tree.root_node().has_error(),
        "{source:?}: {}",
        tree.root_node().to_sexp()
    );
    let constant = exact(tree, source, "const", literal);
    written(constant, source, "const", literal);
    assert_eq!(constant.named_child_count(), 1);
    let numeric = constant.named_child(0).unwrap();
    written(numeric, source, kind, literal);
    assert_eq!(numeric.parent(), Some(constant));
    if let Some((base_kind, base_text)) = base {
        let child = numeric.named_child(0).unwrap();
        written(child, source, base_kind, base_text);
        assert_eq!(child.parent(), Some(numeric));
        assert_eq!(child.start_byte(), numeric.start_byte());
        assert!(child.end_byte() < numeric.end_byte());
    }
    let case = exact(
        tree,
        source,
        "enum_type_case",
        &format!("First = ({literal})"),
    );
    let parenthesis = case.named_child(1).unwrap();
    written(
        parenthesis,
        source,
        "paren_expression",
        &format!("({literal})"),
    );
    assert_eq!(parenthesis.named_child(0), Some(constant));
    assert_eq!(constant.parent(), Some(parenthesis));
    for node in nodes(tree) {
        assert_ne!(node.kind(), "application_expression");
        assert!(!node.kind().contains("literal_suffix"));
        assert!(!node.kind().contains("numeric_digit_continuation"));
        assert!(!node.kind().contains("numeric_exponent_continuation"));
    }
}

#[derive(Debug, PartialEq, Eq)]
struct VisibleNode {
    kind: String,
    flags: [bool; 4],
    bytes: (usize, usize),
    points: (Point, Point),
    fields: Vec<Option<String>>,
}

fn snapshot(tree: &Tree, source: &str) -> Vec<VisibleNode> {
    nodes(tree)
        .into_iter()
        .map(|node| {
            assert!(node.end_byte() <= source.len());
            assert_eq!(node.start_position(), point(source, node.start_byte()));
            assert_eq!(node.end_position(), point(source, node.end_byte()));
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
            }
        })
        .collect()
}

#[test]
fn numeric_literal_components_and_public_owners_survive_in_both_profiles() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for &(literal, kind, base) in LITERALS {
                    let source = fixture(signature, &format!("({literal})"), ending, final_newline);
                    let tree = parse(&mut parser(signature), &source, None);
                    literal_owner(&tree, &source, literal, kind, base);
                    snapshot(&tree, &source);
                }
            }
        }
    }
}

#[test]
fn completed_numeric_argument_links_keep_adjacent_string_and_collection_arguments() {
    for ending in ["\n", "\r\n"] {
        for (target, literal) in [
            ("consume 1u", "1u"),
            ("consume 0x1u", "0x1u"),
            ("consume 1.0", "1.0"),
            ("consume 1\"text\"", "1"),
            ("consume 1[|2|]", "1"),
            ("consume 1{ Value = 2 }", "1"),
        ] {
            let source = format!(
                "module NumericArgumentExample\nlet run () =\n    {target}\nlet following = 30"
            )
            .replace('\n', ending);
            let tree = parse(&mut parser(false), &source, None);
            assert!(
                !tree.root_node().has_error(),
                "{source:?}: {}",
                tree.root_node().to_sexp()
            );
            let application = exact(&tree, &source, "application_expression", target);
            written(application, &source, "application_expression", target);
            let constant = exact(&tree, &source, "const", literal);
            written(constant, &source, "const", literal);
            assert!(constant.start_byte() >= application.start_byte());
            assert!(constant.end_byte() <= application.end_byte());
            snapshot(&tree, &source);
        }
    }
}

#[test]
fn short_identifier_arguments_after_closed_nonnumeric_atoms_remain_applications() {
    for ending in ["\n", "\r\n"] {
        for target in [
            "printfn\"%s\"u",
            "printfn\"%s\"m",
            "printfn\"%s\"f",
            "(consume)u",
            "(consume)m",
            "(consume)f",
        ] {
            let source = format!(
                "module ShortArgumentExample\nlet run u m f =\n    {target}\nlet following = 30"
            )
            .replace('\n', ending);
            let tree = parse(&mut parser(false), &source, None);
            assert!(
                !tree.root_node().has_error(),
                "{source:?}: {}",
                tree.root_node().to_sexp()
            );
            let application = exact(&tree, &source, "application_expression", target);
            let argument = application.named_child(1).unwrap();
            let value = target.get(target.len() - 1..).unwrap();
            written(argument, &source, "long_identifier_or_op", value);
            written(
                argument.named_child(0).unwrap(),
                &source,
                "identifier",
                value,
            );
            assert_eq!(argument.parent(), Some(application));
            snapshot(&tree, &source);
        }
    }
}

fn edit(tree: &mut Tree, before: &str, after: &str) {
    // These fixtures are ASCII, so the byte-common frontier is a UTF-8 boundary.
    assert!(before.is_ascii() && after.is_ascii());
    let prefix = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = before
        .as_bytes()
        .get(prefix..)
        .unwrap()
        .iter()
        .rev()
        .zip(after.as_bytes().get(prefix..).unwrap().iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_end = before.len() - suffix;
    let new_end = after.len() - suffix;
    tree.edit(&InputEdit {
        start_byte: prefix,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point(before, prefix),
        old_end_position: point(before, old_end),
        new_end_position: point(after, new_end),
    });
    assert!(tree.root_node().has_changes());
}

#[test]
fn numeric_continuation_edits_match_fresh_trees_and_restore() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            let original = fixture(signature, "(1u)", ending, false);
            let mut reused = parser(signature);
            let mut tree = parse(&mut reused, &original, None);
            let baseline = snapshot(&tree, &original);
            let mut before = original.clone();
            for rhs in [
                "(1ul)",
                "(0x1u)",
                "(1.2)",
                "(1.23)",
                "(1.23e+4)",
                "(1.23e+45)",
                "(1.e-2)",
                "(1e+)",
                "(1u<<<0)",
                "(1u <<< 0)",
                "(1u(*c*)<<<0)",
                "(1u)",
            ] {
                let after = fixture(signature, rhs, ending, false);
                edit(&mut tree, &before, &after);
                let incremental = parse(&mut reused, &after, Some(&tree));
                let fresh = parse(&mut parser(signature), &after, None);
                assert_eq!(
                    snapshot(&incremental, &after),
                    snapshot(&fresh, &after),
                    "{signature}/{rhs}"
                );
                tree = incremental;
                before = after;
            }
            assert_eq!(before, original);
            assert_eq!(snapshot(&tree, &before), baseline);
        }
    }
}

#[test]
fn malformed_numeric_exponents_recover_without_consuming_next_cases() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            let mut reused = parser(signature);
            for broken in ["1e", "1e+", "1e-"] {
                let source = fixture(signature, &format!("({broken})"), ending, false);
                let tree = parse(&mut reused, &source, None);
                assert!(
                    tree.root_node().has_error(),
                    "{source:?}: {}",
                    tree.root_node().to_sexp()
                );
                let next = exact(&tree, &source, "enum_type_case", "Next = 2");
                written(next.named_child(1).unwrap(), &source, "const", "2");
                let first = source.find("First =").unwrap();
                assert!(first < next.start_byte());
                snapshot(&tree, &source);
                let complete = fixture(signature, "(1e+2)", ending, false);
                let fresh = parse(&mut parser(signature), &complete, None);
                let after_recovery = parse(&mut reused, &complete, None);
                assert_eq!(
                    snapshot(&after_recovery, &complete),
                    snapshot(&fresh, &complete)
                );
            }
        }
    }
}

fn numeric_component(
    constant: Node<'_>,
    source: &str,
    literal: &str,
    kind: &str,
    base: Option<(&str, &str)>,
) {
    written(constant, source, "const", literal);
    assert_eq!(constant.named_child_count(), 1);
    let numeric = constant.named_child(0).unwrap();
    written(numeric, source, kind, literal);
    assert_eq!(numeric.parent(), Some(constant));
    if let Some((base_kind, base_text)) = base {
        assert_eq!(numeric.named_child_count(), 1);
        let child = numeric.named_child(0).unwrap();
        written(child, source, base_kind, base_text);
        assert_eq!(child.parent(), Some(numeric));
        assert_eq!(child.start_byte(), numeric.start_byte());
        assert!(child.end_byte() < numeric.end_byte());
    }
}

fn numeric_context_nodes_remain_hidden(tree: &Tree) {
    for node in nodes(tree) {
        assert!(!node.kind().contains("literal_suffix"));
        assert!(!node.kind().contains("numeric_digit_continuation"));
        assert!(!node.kind().contains("numeric_exponent_continuation"));
        assert!(!matches!(
            node.kind(),
            "_integer_suffix_base"
                | "_based_suffix_base"
                | "_float_suffix_base"
                | "_numeric_literal_digits"
                | "_numeric_literal_exponent"
        ));
    }
}

fn enum_tail<'tree>(tree: &'tree Tree, source: &str, signature: bool) -> Node<'tree> {
    let next = exact(tree, source, "enum_type_case", "Next = 2");
    assert!(!next.has_error());
    assert_eq!(next.named_child_count(), 2);
    written(next.named_child(0).unwrap(), source, "identifier", "Next");
    written(next.named_child(1).unwrap(), source, "const", "2");
    let following = if signature {
        exact(tree, source, "value_definition", "val following: int")
    } else {
        exact(tree, source, "declaration_expression", "let following = 30")
    };
    assert!(!following.has_error());
    assert!(next.end_byte() < following.start_byte());
    assert_eq!(source.find("Next = 2"), Some(next.start_byte()));
    assert_eq!(
        source.find(if signature {
            "val following:"
        } else {
            "let following ="
        }),
        Some(following.start_byte())
    );
    next
}

#[test]
fn bare_numeric_enum_rhs_keeps_direct_const_and_specialized_components() {
    let representatives = [
        ("1u", "uint32", Some(("int", "1"))),
        ("0x1u", "uint32", Some(("xint", "0x1"))),
        ("1.0f", "ieee32", Some(("float", "1.0"))),
        ("1e2", "float", None),
        ("1.e2", "float", None),
        ("1.0", "float", None),
        ("1.23", "float", None),
        ("1.0M", "decimal", Some(("float", "1.0"))),
    ];
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for (literal, kind, base) in representatives {
                    let source = fixture(signature, literal, ending, final_newline);
                    let tree = parse(&mut parser(signature), &source, None);
                    assert!(
                        !tree.root_node().has_error(),
                        "{source:?}: {}",
                        tree.root_node().to_sexp()
                    );
                    let case = exact(
                        &tree,
                        &source,
                        "enum_type_case",
                        &format!("First = {literal}"),
                    );
                    assert_eq!(case.named_child_count(), 2);
                    written(case.named_child(0).unwrap(), &source, "identifier", "First");
                    let constant = case.named_child(1).unwrap();
                    numeric_component(constant, &source, literal, kind, base);
                    assert_eq!(constant.parent(), Some(case));
                    assert_eq!(case.end_byte(), constant.end_byte());
                    assert!(case.end_byte() < enum_tail(&tree, &source, signature).start_byte());
                    assert!(
                        nodes(&tree)
                            .iter()
                            .all(|node| node.kind() != "application_expression")
                    );
                    numeric_context_nodes_remain_hidden(&tree);
                    snapshot(&tree, &source);
                }
            }
        }
    }
}

#[test]
fn unspaced_parenthesized_numeric_shifts_keep_const_infix_const_ownership() {
    let representatives = [
        ("1u", "uint32", Some(("int", "1"))),
        ("0x1u", "uint32", Some(("xint", "0x1"))),
        ("1.0f", "ieee32", Some(("float", "1.0"))),
        ("1e2", "float", None),
        ("1.0", "float", None),
    ];
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for (literal, kind, base) in representatives {
                    let shifted = format!("{literal}<<<0");
                    let rhs = format!("({shifted})");
                    let source = fixture(signature, &rhs, ending, final_newline);
                    let tree = parse(&mut parser(signature), &source, None);
                    assert!(
                        !tree.root_node().has_error(),
                        "{source:?}: {}",
                        tree.root_node().to_sexp()
                    );
                    let case = exact(&tree, &source, "enum_type_case", &format!("First = {rhs}"));
                    assert_eq!(case.named_child_count(), 2);
                    let parenthesis = case.named_child(1).unwrap();
                    written(parenthesis, &source, "paren_expression", &rhs);
                    assert_eq!(parenthesis.named_child_count(), 1);
                    let infix = parenthesis.named_child(0).unwrap();
                    written(infix, &source, "infix_expression", &shifted);
                    assert_eq!(infix.named_child_count(), 3);
                    let left = infix.named_child(0).unwrap();
                    let operator = infix.named_child(1).unwrap();
                    let right = infix.named_child(2).unwrap();
                    numeric_component(left, &source, literal, kind, base);
                    written(operator, &source, "infix_op", "<<<");
                    numeric_component(right, &source, "0", "int", None);
                    for child in [left, operator, right] {
                        assert_eq!(child.parent(), Some(infix));
                    }
                    assert_eq!(left.end_byte(), operator.start_byte());
                    assert_eq!(operator.end_byte(), right.start_byte());
                    assert_eq!(infix.start_byte(), left.start_byte());
                    assert_eq!(infix.end_byte(), right.end_byte());
                    assert_eq!(infix.parent(), Some(parenthesis));
                    assert_eq!(parenthesis.parent(), Some(case));
                    assert_eq!(case.end_byte(), parenthesis.end_byte());
                    assert!(case.end_byte() < enum_tail(&tree, &source, signature).start_byte());
                    assert!(
                        nodes(&tree)
                            .iter()
                            .all(|node| node.kind() != "application_expression")
                    );
                    numeric_context_nodes_remain_hidden(&tree);
                    snapshot(&tree, &source);
                }
            }
        }
    }
}

fn separated_numeric_rhs_recovery(tree: &Tree, source: &str, signature: bool, rhs: &str) {
    // A bare enum RHS is atomic in both profiles; trivia cannot join immediate tokens.
    assert!(
        tree.root_node().has_error(),
        "{source:?}: {}",
        tree.root_node().to_sexp()
    );
    let sexp = tree.root_node().to_sexp();
    assert!(
        sexp.contains("(ERROR") || sexp.contains("(MISSING "),
        "{sexp}"
    );
    let first = source.find("First = ").unwrap() + "First = ".len();
    let split = first + rhs.find(' ').unwrap();
    let end = first + rhs.len();
    assert_eq!(source.get(first..end), Some(rhs));
    let next = enum_tail(tree, source, signature);
    assert!(end < next.start_byte());
    for node in nodes(tree) {
        if matches!(
            node.kind(),
            "const"
                | "int"
                | "xint"
                | "sbyte"
                | "byte"
                | "int16"
                | "uint16"
                | "int32"
                | "uint32"
                | "nativeint"
                | "unativeint"
                | "int64"
                | "uint64"
                | "ieee32"
                | "ieee64"
                | "bignum"
                | "decimal"
                | "float"
        ) && !node.has_error()
            && node.start_byte() < split
            && node.end_byte() > split
        {
            panic!(
                "clean numeric owner crossed immediate-token trivia: {}",
                node.to_sexp()
            );
        }
    }
    numeric_context_nodes_remain_hidden(tree);
    snapshot(tree, source);
}

#[test]
fn trivia_separated_numeric_continuations_recover_without_joining_written_tokens() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                for (separated, joined) in [
                    ("1 u", "1u"),
                    ("0x1 u", "0x1u"),
                    ("1.0 f", "1.0f"),
                    ("1 e2", "1e2"),
                    ("1. 0", "1.0"),
                    ("1e 2", "1e2"),
                ] {
                    let original = fixture(signature, joined, ending, final_newline);
                    let changed = fixture(signature, separated, ending, final_newline);
                    let mut reused = parser(signature);
                    let mut old = parse(&mut reused, &original, None);
                    assert!(
                        !old.root_node().has_error(),
                        "{}",
                        old.root_node().to_sexp()
                    );
                    let first = exact(
                        &old,
                        &original,
                        "enum_type_case",
                        &format!("First = {joined}"),
                    );
                    written(first.named_child(1).unwrap(), &original, "const", joined);
                    let baseline = snapshot(&old, &original);
                    edit(&mut old, &original, &changed);
                    let mut incremental = parse(&mut reused, &changed, Some(&old));
                    let fresh = parse(&mut parser(signature), &changed, None);
                    separated_numeric_rhs_recovery(&incremental, &changed, signature, separated);
                    separated_numeric_rhs_recovery(&fresh, &changed, signature, separated);
                    assert_eq!(snapshot(&incremental, &changed), snapshot(&fresh, &changed));
                    edit(&mut incremental, &changed, &original);
                    let restored = parse(&mut reused, &original, Some(&incremental));
                    assert_eq!(snapshot(&restored, &original), baseline);
                    let fresh_restored = parse(&mut parser(signature), &original, None);
                    assert_eq!(
                        snapshot(&restored, &original),
                        snapshot(&fresh_restored, &original)
                    );
                }
            }
        }
    }
}

#[test]
fn decimal_dots_and_range_tokens_keep_distinct_written_owners() {
    let source = "module NumericRangeExample\nlet run () =\n    for value in 1..2 do ignore value\n    consume 1.\n    consume 1.e2\nlet following = 30";
    for ending in ["\n", "\r\n"] {
        let source = source.replace('\n', ending);
        let tree = parse(&mut parser(false), &source, None);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let range = exact(&tree, &source, "range_expression", "1..2");
        written(range, &source, "range_expression", "1..2");
        written(exact(&tree, &source, "const", "1."), &source, "const", "1.");
        written(
            exact(&tree, &source, "const", "1.e2"),
            &source,
            "const",
            "1.e2",
        );
        snapshot(&tree, &source);
    }
}
