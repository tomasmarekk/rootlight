//! Original native DQL queries retain physical function, predicate and directive roles.
//! The parser is exercised directly; SDK and repository qualification are separate.

use tree_sitter::{Node, Parser, Tree};

const SUITE_ONE: &[u8] = include_bytes!("fixtures/suite01.dql");
const SUITE_TWO: &[u8] = include_bytes!("fixtures/suite02.dql");
const SUITE_THREE: &[u8] = include_bytes!("fixtures/suite03.dql");

fn parse(source: &[u8]) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&rootlight_tree_sitter_dql::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

fn clean(source: &[u8]) -> Tree {
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(root.start_byte(), 0);
    assert_eq!(root.end_byte(), source.len());
    tree
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            found.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    found.sort_by_key(Node::start_byte);
    found
}

fn text<'source>(node: Node<'_>, source: &'source [u8]) -> &'source str {
    node.utf8_text(source).unwrap()
}

fn field_text<'source>(node: Node<'_>, field: &str, source: &'source [u8]) -> &'source str {
    text(
        node.child_by_field_name(field)
            .unwrap_or_else(|| panic!("missing {field} on {}", node.to_sexp())),
        source,
    )
}

#[test]
fn original_first_suite_preserves_two_native_generator_roots() {
    assert_eq!(SUITE_ONE.len(), 106);
    let tree = clean(SUITE_ONE);
    let roots = nodes(tree.root_node(), "root_query");
    assert_eq!(roots.len(), 2);
    assert_eq!(field_text(roots[0], "name", SUITE_ONE), "all");
    assert_eq!(field_text(roots[1], "name", SUITE_ONE), "any");
    let functions = nodes(tree.root_node(), "root_function");
    assert_eq!(functions.len(), 2);
    assert_eq!(field_text(functions[0], "name", SUITE_ONE), "allofterms");
    assert_eq!(field_text(functions[1], "name", SUITE_ONE), "anyofterms");
    for function in functions {
        let arguments = function.child_by_field_name("arguments").unwrap();
        assert_eq!(field_text(arguments, "predicate", SUITE_ONE), "name");
    }
    assert_eq!(nodes(tree.root_node(), "string").len(), 2);
}

#[test]
fn original_second_suite_preserves_filter_and_reverse_count_ownership() {
    assert_eq!(SUITE_TWO.len(), 1360);
    let tree = clean(SUITE_TWO);
    assert_eq!(nodes(tree.root_node(), "root_query").len(), 18);
    assert_eq!(nodes(tree.root_node(), "count_selection").len(), 6);
    let filters = nodes(tree.root_node(), "filter_directive");
    assert_eq!(filters.len(), 12);
    for (index, filter) in filters.into_iter().enumerate() {
        assert_eq!(field_text(filter, "name", SUITE_TWO), "filter");
        let functions = nodes(filter, "function_call");
        assert_eq!(functions.len(), 1);
        assert_eq!(field_text(functions[0], "name", SUITE_TWO), "eq");
        let counts = nodes(filter, "nested_function");
        assert_eq!(counts.len(), 1);
        assert_eq!(field_text(counts[0], "name", SUITE_TWO), "count");
        let expected = if index < 6 { "friend" } else { "~friend" };
        assert_eq!(field_text(counts[0], "argument", SUITE_TWO), expected);
        let predicate = nodes(counts[0], "predicate_name")[0];
        assert_eq!(field_text(predicate, "name", SUITE_TWO), "friend");
        assert_eq!(
            predicate.child_by_field_name("reverse").is_some(),
            index >= 6
        );
    }
}

#[test]
fn original_third_suite_preserves_native_default_language_suffix() {
    let tree = clean(SUITE_THREE);
    assert_eq!(nodes(tree.root_node(), "root_query").len(), 4);
    assert_eq!(nodes(tree.root_node(), "count_selection").len(), 1);
    let languages = nodes(tree.root_node(), "function_language");
    assert_eq!(languages.len(), 1);
    assert_eq!(text(languages[0], SUITE_THREE), "@.");
    let predicate = languages[0].parent().unwrap();
    assert_eq!(predicate.kind(), "predicate");
    assert_eq!(text(predicate, SUITE_THREE), "name@.");
    assert!(nodes(tree.root_node(), "filter_directive").is_empty());
}

#[test]
fn named_query_bindings_aliases_and_filter_precedence_keep_distinct_fields() {
    let source = br#"query find($term: string = "value") {
      q(func: eq(name, $term), first: 10)
        @filter(a(name, "$term") OR b(name, "x") AND NOT c(name, "y")) {
        f as bestFriend: friends(first: 10) { c as count(~friend) }
        name@en:ru
      }
      friend(func: uid(f)) { fcount: val(c) }
    }"#;
    let tree = clean(source);
    let root = tree.root_node();
    assert_eq!(
        field_text(nodes(root, "query_operation")[0], "name", source),
        "find"
    );
    assert_eq!(nodes(root, "variable_reference").len(), 2);
    let bindings: Vec<_> = nodes(root, "variable_binding")
        .into_iter()
        .map(|node| field_text(node, "name", source))
        .collect();
    assert_eq!(bindings, ["f", "c"]);
    let aliases: Vec<_> = nodes(root, "output_alias")
        .into_iter()
        .map(|node| field_text(node, "name", source))
        .collect();
    assert_eq!(aliases, ["bestFriend", "fcount"]);
    assert_eq!(text(nodes(root, "language_suffix")[0], source), "@en:ru");
    let binary = nodes(root, "binary_expression");
    let outer = binary
        .iter()
        .copied()
        .find(|node| field_text(*node, "operator", source) == "OR")
        .unwrap();
    let right = outer.child_by_field_name("right").unwrap();
    assert_eq!(right.kind(), "binary_expression");
    assert_eq!(field_text(right, "operator", source), "AND");
    let not = right.child_by_field_name("right").unwrap();
    assert_eq!(not.kind(), "unary_expression");
    assert_eq!(field_text(not, "operator", source), "NOT");
}

#[test]
fn native_at_dispatch_keeps_language_suffixes_and_filter_calls_distinct() {
    // Pinned selection dispatch treats bare filter as a language, but filter(...)
    // as a directive; root directives never have that language fallback.
    for (source, suffix) in [
        (
            br#"{ q(func: uid(1)) { name@filter } }"#.as_slice(),
            "@filter",
        ),
        (
            br#"{ q(func: uid(1)) { name@en @filter(eq(name, "x")) } }"#.as_slice(),
            "@en",
        ),
    ] {
        let tree = clean(source);
        let languages = nodes(tree.root_node(), "language_suffix");
        assert_eq!(languages.len(), 1);
        assert_eq!(text(languages[0], source), suffix);
        let filters = nodes(tree.root_node(), "filter_directive");
        assert_eq!(filters.len(), usize::from(suffix == "@en"));
        for filter in filters {
            assert_eq!(filter.parent().unwrap().kind(), "field_selection");
            assert_eq!(field_text(filter, "name", source), "filter");
        }
    }
    assert!(
        parse(b"{ q(func: uid(1)) @filter { name } }")
            .root_node()
            .has_error()
    );
}

#[test]
fn strict_argument_and_native_root_boundaries_refuse_malformed_input() {
    for source in [
        r#"{ q(allofterms(name, "x")) { name } }"#,
        r#"{ q(func: allofterms("name", "x")) { name } }"#,
        r#"{ q(func: allofterms()) { name } }"#,
        r#"{ q(func: uid(1),) { name } }"#,
        r#"{ q(func: eq(name, "x",)) { name } }"#,
        r#"{ q(func: uid(1)) @filter(eq(name,,)) { name } }"#,
        r#"{ q(func: uid(1)) @filter(a(name "x")) { name } }"#,
        r#"{ q(func: uid(1)) @filter(a(name, "x") AND) { name } }"#,
        r#"{ q(func: uid(1)) { count(friend, other) } }"#,
        r#"{ q(func: allofterms(name@en:ru, "x")) { name } }"#,
        r#"{ user(id: 1) { name } }"#,
        r#"{"query":"{ q(func: uid(1)) { name } }"}"#,
        "friend: [uid] @count @reverse .",
    ] {
        assert!(
            parse(source.as_bytes()).root_node().has_error(),
            "accepted {source}"
        );
    }
}

#[test]
fn native_string_escapes_and_physical_cr_nul_comments_retain_source_ranges() {
    for literal in [r#""\v\u0000\u03BB\"\\""#, "\"raw\0value\rλ😀\""] {
        let source =
            format!("# comment\0\r\n{{ q(func: eq(name, {literal})) {{ name }} }} # eof\0");
        let tree = clean(source.as_bytes());
        let strings = nodes(tree.root_node(), "string");
        assert_eq!(strings.len(), 1);
        assert_eq!(text(strings[0], source.as_bytes()), literal);
        assert_eq!(nodes(tree.root_node(), "comment").len(), 2);
    }
    for literal in [
        r#""\x41""#,
        r#""\U0001F600""#,
        r#""\/""#,
        r#""\'""#,
        r#""\uD800""#,
        r#""\u00XX""#,
        "\"raw\nline\"",
        "\"unterminated",
    ] {
        let source = format!("{{ q(func: eq(name, {literal})) {{ name }} }}");
        assert!(
            parse(source.as_bytes()).root_node().has_error(),
            "accepted {literal:?}"
        );
    }
}

#[test]
fn empty_uid_and_native_aggregation_roots_keep_different_roles() {
    let source = b"{ q(func: uid()) { name } totals() { sum(val(a)) avg(val(a)) } }";
    let tree = clean(source);
    assert_eq!(nodes(tree.root_node(), "root_query").len(), 1);
    assert_eq!(nodes(tree.root_node(), "aggregation_root").len(), 1);
    let aggregate_names: Vec<_> = nodes(tree.root_node(), "aggregate_selection")
        .into_iter()
        .map(|node| field_text(node, "name", source))
        .collect();
    assert_eq!(aggregate_names, ["sum", "avg"]);
    assert!(parse(b"{ totals() { name } }").root_node().has_error());
}
