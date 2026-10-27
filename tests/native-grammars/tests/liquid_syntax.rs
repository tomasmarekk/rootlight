//! Native CST regressions for dynamic Liquid access and the unchanged corpus fixture.
//! These controls require complete clean trees; they establish no adapter IR,
//! lexical runtime target, cancellation contract, or corpus coverage outcome.

use std::ops::Range;

use tree_sitter::{Node, Parser, Tree};

const ORIGINAL: &[u8] = include_bytes!(
    "../../../crates/rootlight-adapter-treesitter/tests/fixtures/liquid/blog-sidebar.liquid"
);

struct Healthy;
impl tree_sitter_liquid::CancellationProbe for Healthy {
    fn is_cancelled(&self) -> bool {
        false
    }
}

fn parse(source: &[u8]) -> Tree {
    let scope = tree_sitter_liquid::ScannerScope::begin(Healthy, 2_000_000).unwrap();
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_liquid::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    drop(parser);
    scope.finish().unwrap();
    tree
}

fn nodes(tree: &Tree) -> Vec<Node<'_>> {
    let mut result = Vec::new();
    let mut cursor = tree.root_node().walk();
    loop {
        result.push(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return result;
            }
        }
    }
}

fn clean(source: &[u8]) -> Tree {
    let tree = parse(source);
    // Whole input is certified by ranges; native root extents may exclude trivia.
    assert!(matches!(tree.included_ranges().as_slice(), [range]
        if range.start_byte == 0 && range.end_byte >= source.len()));
    assert!(tree.root_node().end_byte() <= source.len());
    assert!(
        !tree.root_node().has_error(),
        "the complete source must parse without recovery: {}",
        tree.root_node().to_sexp()
    );
    assert!(
        nodes(&tree)
            .iter()
            .all(|node| !node.is_error() && !node.is_missing())
    );
    tree
}

fn one_at<'tree>(tree: &'tree Tree, kind: &str, span: Range<usize>) -> Node<'tree> {
    let found: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == kind && node.byte_range() == span)
        .collect();
    assert_eq!(found.len(), 1, "expected one {kind} at {span:?}");
    found[0]
}

fn literal_span(source: &[u8], value: &[u8]) -> Range<usize> {
    let found: Vec<_> = source
        .windows(value.len())
        .enumerate()
        .filter_map(|(start, part)| (part == value).then_some(start))
        .collect();
    assert_eq!(found.len(), 1, "the fixed test endpoint must be unique");
    found[0]..found[0] + value.len()
}

fn field<'tree>(owner: Node<'tree>, name: &str) -> Node<'tree> {
    owner.child_by_field_name(name).unwrap()
}

fn loop_at<'tree>(tree: &'tree Tree, item_span: Range<usize>) -> Node<'tree> {
    let found: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| {
            node.kind() == "for_loop_statement"
                && node
                    .child_by_field_name("item")
                    .is_some_and(|item| item.byte_range() == item_span)
        })
        .collect();
    assert_eq!(found.len(), 1);
    found[0]
}

fn nearest_loop(mut node: Node<'_>) -> Node<'_> {
    loop {
        node = node.parent().unwrap();
        if node.kind() == "for_loop_statement" {
            return node;
        }
    }
}

fn assert_body_contains(owner: Node<'_>, child: Node<'_>) {
    let body = field(owner, "body");
    assert_eq!(body.kind(), "block");
    assert!(body.start_byte() <= child.start_byte() && child.end_byte() <= body.end_byte());
    assert_eq!(nearest_loop(child).id(), owner.id());
}

fn assert_output_operand(node: Node<'_>) {
    assert!(matches!(node.prev_sibling().unwrap().kind(), "{{" | "{{-"));
    assert!(matches!(node.next_sibling().unwrap().kind(), "}}" | "-}}"));
}

#[test]
fn original_complete_source_keeps_both_loop_items_and_value_receivers() {
    assert_eq!(ORIGINAL.len(), 888);
    let tree = clean(ORIGINAL);
    let article = one_at(&tree, "identifier", 191..198);
    let tag = one_at(&tree, "identifier", 673..676);
    assert_eq!(article.utf8_text(ORIGINAL).unwrap(), "article");
    assert_eq!(tag.utf8_text(ORIGINAL).unwrap(), "tag");
    let article_loop = loop_at(&tree, 191..198);
    let tag_loop = loop_at(&tree, 673..676);
    assert_ne!(article_loop.id(), tag_loop.id());
    assert_eq!(field(article_loop, "item").id(), article.id());
    assert_eq!(field(tag_loop, "item").id(), tag.id());
    assert_eq!(
        field(article_loop, "iterator").utf8_text(ORIGINAL).unwrap(),
        "blogs[blog.handle].articles"
    );
    assert_eq!(
        field(article_loop, "modifier").utf8_text(ORIGINAL).unwrap(),
        "limit:6"
    );
    let article_use = one_at(&tree, "identifier", 270..277);
    let article_access = article_use.parent().unwrap();
    assert_eq!(article_access.kind(), "access");
    assert_eq!(field(article_access, "receiver").id(), article_use.id());
    assert_eq!(
        field(article_access, "property")
            .utf8_text(ORIGINAL)
            .unwrap(),
        "url"
    );
    assert_body_contains(article_loop, article_use);
    assert_output_operand(article_access);
    let tag_use = one_at(&tree, "identifier", 753..756);
    assert_body_contains(tag_loop, tag_use);
    assert_output_operand(tag_use);
    assert_ne!(tag_use.id(), field(tag_loop, "item").id());
    assert_eq!(
        nodes(&tree)
            .into_iter()
            .filter(|node| node.kind() == "for_loop_statement")
            .count(),
        2
    );
}

#[test]
fn identifier_bracket_key_has_a_native_property_field() {
    let source = b"{{ buckets[selection] }}";
    let tree = clean(source);
    let access = one_at(&tree, "access", literal_span(source, b"buckets[selection]"));
    assert_eq!(
        field(access, "receiver").utf8_text(source).unwrap(),
        "buckets"
    );
    let key = field(access, "property");
    assert_eq!(key.kind(), "identifier");
    assert_eq!(key.byte_range(), literal_span(source, b"selection"));
    assert_output_operand(access);
}

#[test]
fn member_bracket_key_preserves_its_own_receiver_and_property() {
    let source = b"{{ blogs[blog.handle].articles }}";
    let tree = clean(source);
    let outer = one_at(
        &tree,
        "access",
        literal_span(source, b"blogs[blog.handle].articles"),
    );
    assert_eq!(
        field(outer, "property").utf8_text(source).unwrap(),
        "articles"
    );
    let indexed = field(outer, "receiver");
    assert_eq!(indexed.utf8_text(source).unwrap(), "blogs[blog.handle]");
    let key = field(indexed, "property");
    assert_eq!(key.kind(), "access");
    assert_eq!(field(key, "receiver").utf8_text(source).unwrap(), "blog");
    assert_eq!(field(key, "property").utf8_text(source).unwrap(), "handle");
    assert_output_operand(outer);
}

#[test]
fn chained_and_nested_dynamic_brackets_keep_distinct_native_owners() {
    let source = b"{{ catalogs[settings.primary][selectors[index]].title }}";
    let tree = clean(source);
    let outer = one_at(
        &tree,
        "access",
        literal_span(
            source,
            b"catalogs[settings.primary][selectors[index]].title",
        ),
    );
    let indexed = field(outer, "receiver");
    let second_key = field(indexed, "property");
    assert_eq!(second_key.kind(), "access");
    assert_eq!(
        field(second_key, "receiver").utf8_text(source).unwrap(),
        "selectors"
    );
    assert_eq!(
        field(second_key, "property").utf8_text(source).unwrap(),
        "index"
    );
    let first = field(indexed, "receiver");
    let first_key = field(first, "property");
    assert_eq!(first_key.kind(), "access");
    assert_eq!(
        field(first_key, "receiver").utf8_text(source).unwrap(),
        "settings"
    );
    assert_eq!(
        field(first_key, "property").utf8_text(source).unwrap(),
        "primary"
    );
    assert_ne!(first_key.id(), second_key.id());
    assert_output_operand(outer);
}

#[test]
fn literal_string_and_numeric_brackets_retain_their_existing_node_kinds() {
    let source = b"{{ names['primary'].title }} {{ matrix[2][0] }}";
    let tree = clean(source);
    let named = one_at(&tree, "access", literal_span(source, b"names['primary']"));
    assert_eq!(field(named, "property").kind(), "string");
    assert_eq!(
        field(named, "property").utf8_text(source).unwrap(),
        "'primary'"
    );
    let numeric = one_at(&tree, "access", literal_span(source, b"matrix[2][0]"));
    assert_eq!(field(numeric, "property").kind(), "number");
    assert_eq!(field(numeric, "property").utf8_text(source).unwrap(), "0");
    let first_numeric = field(numeric, "receiver");
    assert_eq!(field(first_numeric, "property").kind(), "number");
    assert_eq!(
        field(first_numeric, "property").utf8_text(source).unwrap(),
        "2"
    );
}

#[test]
fn raw_and_comment_contents_do_not_materialize_lookalike_loop_or_access_nodes() {
    let source = b"{% raw %}{% for ghost in things[key] %}{{ ghost.url }}{% endfor %}{% endraw %}{% comment %}{{ things[selector] }}{% endcomment %}";
    let tree = clean(source);
    assert!(
        nodes(&tree)
            .into_iter()
            .all(|node| !matches!(node.kind(), "for_loop_statement" | "access" | "identifier"))
    );
    assert!(
        nodes(&tree)
            .into_iter()
            .any(|node| node.kind() == "raw_content")
    );
    assert!(
        nodes(&tree)
            .into_iter()
            .any(|node| node.kind() == "comment")
    );
}

#[test]
fn quoted_output_text_does_not_create_dynamic_access_nodes() {
    let source = b"{{ 'blogs[blog.handle]' }} {{ \"catalogs[index]\" }}";
    let tree = clean(source);
    assert!(
        nodes(&tree)
            .into_iter()
            .all(|node| !matches!(node.kind(), "access" | "identifier" | "for_loop_statement"))
    );
    assert_eq!(
        nodes(&tree)
            .into_iter()
            .filter(|node| node.kind() == "string")
            .count(),
        2
    );
}

#[test]
fn malformed_bracket_expressions_do_not_satisfy_complete_clean_tree_admission() {
    for source in [
        b"{{ things[] }}".as_slice(),
        b"{{ things[selection }}",
        b"{{ things[selection.] }}",
        b"{{ things[[selection]] }}",
    ] {
        let tree = parse(source);
        assert!(
            tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn nested_same_named_loop_items_and_output_owners_remain_distinct() {
    let source = b"{% for item in outer[outer_key] %}{% for item in inner[inner_key] %}{{ item.inner }}{% endfor %}{{ item.outer }}{% endfor %}";
    let tree = clean(source);
    let loops: Vec<_> = nodes(&tree)
        .into_iter()
        .filter(|node| node.kind() == "for_loop_statement")
        .collect();
    assert_eq!(loops.len(), 2);
    assert_ne!(
        field(loops[0], "item").byte_range(),
        field(loops[1], "item").byte_range()
    );
    let inner_output = one_at(&tree, "access", literal_span(source, b"item.inner"));
    let outer_output = one_at(&tree, "access", literal_span(source, b"item.outer"));
    assert_eq!(nearest_loop(inner_output).id(), loops[1].id());
    assert_eq!(nearest_loop(outer_output).id(), loops[0].id());
    assert_body_contains(loops[1], inner_output);
    assert_body_contains(loops[0], outer_output);
}

#[test]
fn whitespace_control_and_limit_modifier_keep_dynamic_iterator_and_output_fields() {
    let source =
        b"{%- for item in buckets[settings.key] limit:6 -%}{{- item.title -}}{%- endfor -%}";
    let tree = clean(source);
    let owner = loop_at(
        &tree,
        literal_span(source, b"item in").start..literal_span(source, b"item in").start + 4,
    );
    assert_eq!(
        field(owner, "iterator").utf8_text(source).unwrap(),
        "buckets[settings.key]"
    );
    assert_eq!(
        field(owner, "modifier").utf8_text(source).unwrap(),
        "limit:6"
    );
    let value = one_at(&tree, "access", literal_span(source, b"item.title"));
    assert_body_contains(owner, value);
    assert_output_operand(value);
}

#[test]
fn native_utf8_byte_endpoints_survive_non_ascii_template_text_and_crlf() {
    let source =
        "<p>λ😀</p>\r\n{% for item in sources[key] %}{{ item.url }}{% endfor %}\r\n".as_bytes();
    let tree = clean(source);
    let access = one_at(&tree, "access", literal_span(source, b"item.url"));
    let receiver = field(access, "receiver");
    assert_eq!(
        receiver.byte_range(),
        access.start_byte()..access.start_byte() + 4
    );
    assert_eq!(receiver.utf8_text(source).unwrap(), "item");
    assert_output_operand(access);
}

#[test]
fn a_missing_endfor_never_becomes_a_complete_clean_native_loop() {
    let tree = parse(b"{% for item in sources[key] %}{{ item.url }}");
    assert!(
        tree.root_node().has_error()
            || nodes(&tree)
                .into_iter()
                .all(|node| node.kind() != "for_loop_statement")
    );
}
