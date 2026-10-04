//! Authored native controls for noninterpolated static string type leaves.
//! Inline nullable annotations retain the compiler-proven single-clause ownership;
//! generic two-arm layout remains outside this leaf repair.

use std::{
    ops::ControlFlow,
    time::{Duration, Instant},
};
use tree_sitter::{Node, ParseOptions, Parser, Point, Tree};

fn parse(signature: bool, source: &str) -> Tree {
    let mut parser = Parser::new();
    let language = if signature {
        tree_sitter_fsharp::LANGUAGE_SIGNATURE
    } else {
        tree_sitter_fsharp::LANGUAGE_FSHARP
    };
    parser
        .set_language(&language.into())
        .expect("pinned native grammar is incompatible with Tree-sitter");
    let started = Instant::now();
    let mut checks = 0_usize;
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
            None,
            Some(ParseOptions::new().progress_callback(&mut watchdog)),
        )
        .expect("authored fixture exceeded its parse watchdog")
}

fn nodes<'tree>(tree: &'tree Tree, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![tree.root_node()];
    let mut found = Vec::new();
    let mut visited = 0_usize;
    while let Some(node) = pending.pop() {
        visited += 1;
        assert!(visited < 100_000, "authored inventory exceeds its bound");
        if node.kind() == kind {
            found.push(node);
        }
        for index in (0..node.child_count()).rev() {
            let index = u32::try_from(index).expect("fixture child index exceeds native API");
            pending.push(node.child(index).expect("advertised child is absent"));
        }
    }
    found
}

fn point(source: &str, offset: usize) -> Point {
    let prefix = &source.as_bytes()[..offset];
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn clean(tree: &Tree, source: &str) {
    assert_eq!(tree.root_node().kind(), "file");
    assert_eq!(tree.root_node().end_byte(), source.len());
    assert!(
        !tree.root_node().has_error(),
        "{source:?}: {}",
        tree.root_node().to_sexp()
    );
    let mut pending = vec![tree.root_node()];
    let mut visited = 0_usize;
    while let Some(node) = pending.pop() {
        visited += 1;
        assert!(visited < 100_000, "authored inventory exceeds its bound");
        assert!(!node.is_error() && !node.is_missing());
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
        for index in (0..node.child_count()).rev() {
            let index = u32::try_from(index).expect("fixture child index exceeds native API");
            pending.push(node.child(index).expect("advertised child is absent"));
        }
    }
}

fn written(node: Node<'_>, source: &str, kind: &str, text: &str) {
    assert_eq!(node.kind(), kind);
    assert_eq!(source.get(node.byte_range()), Some(text));
    assert_eq!(node.start_position(), point(source, node.start_byte()));
    assert_eq!(node.end_position(), point(source, node.end_byte()));
}

fn exact<'tree>(tree: &'tree Tree, source: &str, kind: &str, text: &str) -> Node<'tree> {
    let found: Vec<_> = nodes(tree, kind)
        .into_iter()
        .filter(|node| source.get(node.byte_range()) == Some(text))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{kind} {text:?}: {}",
        tree.root_node().to_sexp()
    );
    written(found[0], source, kind, text);
    found[0]
}

fn field<'tree>(
    node: Node<'tree>,
    source: &str,
    name: &str,
    kind: &str,
    text: &str,
) -> Node<'tree> {
    let child = node
        .child_by_field_name(name)
        .expect("written field is absent");
    written(child, source, kind, text);
    assert_eq!(child.parent(), Some(node));
    child
}

fn child<'tree>(
    node: Node<'tree>,
    source: &str,
    index: u32,
    kind: &str,
    text: &str,
) -> Node<'tree> {
    let child = node
        .named_child(index)
        .expect("written named child is absent");
    written(child, source, kind, text);
    assert_eq!(child.parent(), Some(node));
    child
}

fn variants(fixture: &str, mut witness: impl FnMut(&str)) {
    let without_final_newline = fixture
        .strip_suffix('\n')
        .expect("authored fixture must have a final newline");
    for newline in ["\n", "\r\n"] {
        for text in [without_final_newline, fixture] {
            witness(&text.replace('\n', newline));
        }
    }
}

#[test]
fn inline_nullable_function_annotations_keep_the_compiler_single_clause() {
    let fixtures = [
        (
            "module Example\nlet choose input = match input with | \"x\" -> \"\": string | null -> \"fallback\"\n",
            "\"\": string | null -> \"fallback\"",
            "\"fallback\"",
            "static_constant_type",
        ),
        (
            "module Example\nlet choose input = match input with | \"x\" -> \"\": string | null -> fallback\n",
            "\"\": string | null -> fallback",
            "fallback",
            "simple_type",
        ),
        (
            "module Example\nlet choose = function | \"x\" -> \"\": string | null -> \"fallback\"\n",
            "\"\": string | null -> \"fallback\"",
            "\"fallback\"",
            "static_constant_type",
        ),
        (
            "module Example\nlet choose input = try input with | \"x\" -> \"\": string | null -> \"fallback\"\n",
            "\"\": string | null -> \"fallback\"",
            "\"fallback\"",
            "static_constant_type",
        ),
        (
            "module Example\nlet choose input = match input with | \"x\" -> value : string | null -> \"fallback\"\n",
            "value : string | null -> \"fallback\"",
            "\"fallback\"",
            "static_constant_type",
        ),
    ];
    for (fixture, result, return_type, return_kind) in fixtures {
        variants(fixture, |source| {
            let tree = parse(false, source);
            clean(&tree, source);
            let rules = nodes(&tree, "rule");
            assert_eq!(rules.len(), 1);
            let rule_text = format!("\"x\" -> {result}");
            written(rules[0], source, "rule", &rule_text);
            field(rules[0], source, "pattern", "const", "\"x\"");
            let block = field(rules[0], source, "block", "typecast_expression", result);
            let target = format!("string | null -> {return_type}");
            let function = child(block, source, 1, "function_type", &target);
            let nullable = child(function, source, 0, "nullable_type", "string | null");
            child(nullable, source, 0, "simple_type", "string");
            let rhs = child(function, source, 1, return_kind, return_type);
            if return_kind == "static_constant_type" {
                child(rhs, source, 0, "string", return_type);
                assert_eq!(nodes(&tree, "static_constant_type").len(), 1);
            } else {
                assert!(nodes(&tree, "static_constant_type").is_empty());
            }
            assert_eq!(nodes(&tree, "nullable_type").len(), 1);
        });
    }
}

#[test]
fn type_only_parentheses_do_not_terminate_an_inline_type_annotation() {
    let fixture = "module Example\nlet choose input = match input with | \"x\" -> \"\": (string | null) | null -> \"fallback\"\n";
    variants(fixture, |source| {
        let tree = parse(false, source);
        clean(&tree, source);
        let rules = nodes(&tree, "rule");
        assert_eq!(rules.len(), 1);
        written(
            rules[0],
            source,
            "rule",
            "\"x\" -> \"\": (string | null) | null -> \"fallback\"",
        );
        field(rules[0], source, "pattern", "const", "\"x\"");
        let block = field(
            rules[0],
            source,
            "block",
            "typecast_expression",
            "\"\": (string | null) | null -> \"fallback\"",
        );
        let function = child(
            block,
            source,
            1,
            "function_type",
            "(string | null) | null -> \"fallback\"",
        );
        let outer = child(
            function,
            source,
            0,
            "nullable_type",
            "(string | null) | null",
        );
        let parens = child(outer, source, 0, "paren_type", "(string | null)");
        child(parens, source, 0, "nullable_type", "string | null");
        let rhs = child(function, source, 1, "static_constant_type", "\"fallback\"");
        child(rhs, source, 0, "string", "\"fallback\"");
        assert_eq!(nodes(&tree, "nullable_type").len(), 2);
    });
}

#[test]
fn static_string_types_preserve_literal_and_existing_parameter_ownership() {
    for (literal, literal_kind) in [
        ("\"argument\"", "string"),
        ("@\"argument\"", "verbatim_string"),
        ("\"\"\"argument\"\"\"", "triple_quoted_string"),
    ] {
        for signature in [false, true] {
            let alias = format!("module Example\ntype Literal = {literal}\n");
            variants(&alias, |source| {
                let tree = parse(signature, source);
                clean(&tree, source);
                let leaf = exact(&tree, source, "static_constant_type", literal);
                child(leaf, source, 0, literal_kind, literal);
                assert_eq!(
                    leaf.parent().map(|parent| parent.kind()),
                    Some("type_abbrev_defn")
                );
                assert_eq!(nodes(&tree, "static_constant_type").len(), 1);
                assert!(nodes(&tree, "const").is_empty());
            });
            let function = if signature {
                format!("module Example\nval text : (string -> {literal})\n")
            } else {
                format!("module Example\nlet text = value : string -> {literal}\n")
            };
            variants(&function, |source| {
                let tree = parse(signature, source);
                clean(&tree, source);
                let target = format!("string -> {literal}");
                let function = exact(&tree, source, "function_type", &target);
                child(function, source, 0, "simple_type", "string");
                let leaf = child(function, source, 1, "static_constant_type", literal);
                child(leaf, source, 0, literal_kind, literal);
                assert_eq!(nodes(&tree, "static_constant_type").len(), 1);
            });
            // Separate '<' from '@"' so FCS does not lex a quotation opener.
            let separator = if literal_kind == "verbatim_string" {
                " "
            } else {
                ""
            };
            let parameter = format!("module Example\ntype Provider = P<{separator}{literal}>\n");
            variants(&parameter, |source| {
                let tree = parse(signature, source);
                clean(&tree, source);
                let parameter = exact(&tree, source, "static_parameter_value", literal);
                let constant = child(parameter, source, 0, "const", literal);
                child(constant, source, 0, literal_kind, literal);
                assert!(nodes(&tree, "static_constant_type").is_empty());
            });
        }
    }
}

#[test]
fn expression_only_unit_and_interpolation_do_not_become_static_string_types() {
    for signature in [false, true] {
        for literal in ["()", "$\"argument\""] {
            let fixture = format!("module Example\ntype Literal = {literal}\n");
            variants(&fixture, |source| {
                let tree = parse(signature, source);
                assert!(
                    tree.root_node().has_error(),
                    "{source:?}: {}",
                    tree.root_node().to_sexp()
                );
                assert!(nodes(&tree, "static_constant_type").is_empty());
            });
        }
    }
}

#[test]
fn bare_nullable_annotations_and_typed_heads_remain_available() {
    let fixtures = [
        ("module Example\nlet text = value : string | null\n", 1),
        (
            "module Example\nlet choose input = match input with | \"x\" -> input : string | null\n",
            1,
        ),
        (
            "module Example\nlet text = value : string | null -> string\n",
            1,
        ),
        ("module Example\nlet text : string | null = null\n", 1),
        (
            "module Example\nlet preserve (input : string | null) : string | null = input\n",
            2,
        ),
    ];
    for (fixture, expected) in fixtures {
        variants(fixture, |source| {
            let tree = parse(false, source);
            clean(&tree, source);
            let nullable = nodes(&tree, "nullable_type");
            assert_eq!(nullable.len(), expected);
            for node in nullable {
                written(node, source, "nullable_type", "string | null");
                child(node, source, 0, "simple_type", "string");
            }
            assert!(nodes(&tree, "static_constant_type").is_empty());
        });
    }
}
