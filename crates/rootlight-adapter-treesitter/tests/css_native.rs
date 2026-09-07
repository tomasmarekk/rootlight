//! Native CSS boundaries are tested against exact UTF-8 spans and clean reparses.
//! These syntax contracts do not claim cascade or reference resolution support.

use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

#[test]
fn css_unicode_escapes_and_descendants_preserve_exact_incremental_trees() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_css::LANGUAGE.into())
        .expect("audited ABI loads");
    let cases = [
        ("ascii_rule", ".card { color: red; }", "class_name", "card"),
        (
            "latin1_class",
            ".café { color: red; }",
            "class_name",
            "café",
        ),
        ("greek_class", ".α { color: red; }", "class_name", "α"),
        ("astral_class", ".🦀 { color: red; }", "class_name", "🦀"),
        ("cjk_class", ".名前 { color: red; }", "class_name", "名前"),
        (
            "non_ascii_boundary",
            ".\u{80} { color: red; }",
            "class_name",
            "\u{80}",
        ),
        (
            "unicode_maximum",
            ".\u{10ffff} { color: red; }",
            "class_name",
            "\u{10ffff}",
        ),
        (
            "non_ascii_space_is_name",
            ".a\u{2003}.b { color: red; }",
            "class_name",
            "a\u{2003}",
        ),
        (
            "nbsp_descendant",
            "main \u{a0}child { color: red; }",
            "tag_name",
            "\u{a0}child",
        ),
        (
            "escaped_property",
            ":root { --\\61 ccent: red; }",
            "property_name",
            "--\\61 ccent",
        ),
        (
            "escaped_keyframes",
            "@keyframes \\66 ade { from { opacity: 0; } }",
            "keyframes_name",
            "\\66 ade",
        ),
        ("unicode_at_keyword", "@α {}", "at_keyword", "@α"),
        (
            "escaped_id",
            "#\\31 23 { color: red; }",
            "id_name",
            "\\31 23",
        ),
        (
            "ascii_descendant",
            "main child { color: red; }",
            "tag_name",
            "child",
        ),
        (
            "underscore_descendant",
            "main _child { color: red; }",
            "tag_name",
            "_child",
        ),
        (
            "latin1_descendant",
            "main élément { color: red; }",
            "tag_name",
            "élément",
        ),
        (
            "greek_descendant",
            "main α { color: red; }",
            "tag_name",
            "α",
        ),
        (
            "astral_descendant",
            "main 🦀 { color: red; }",
            "tag_name",
            "🦀",
        ),
        (
            "escaped_class",
            r".\31 23 { color: red; }",
            "class_name",
            r"\31 23",
        ),
        (
            "escaped_tag",
            r"main \65 lement { color: red; }",
            "tag_name",
            r"\65 lement",
        ),
        (
            "custom_property",
            ":root { --accent: red; }",
            "property_name",
            "--accent",
        ),
        (
            "unicode_custom_property",
            ":root { --α: red; }",
            "property_name",
            "--α",
        ),
        (
            "keyframes",
            "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }",
            "keyframes_name",
            "fade",
        ),
        (
            "unicode_keyframes",
            "@keyframes α { from { opacity: 0; } to { opacity: 1; } }",
            "keyframes_name",
            "α",
        ),
        (
            "nested_rule",
            ".card { &:hover { color: red; } }",
            "nesting_selector",
            "&",
        ),
        (
            "scope",
            "@scope (.card) { .title { color: red; } }",
            "class_name",
            "title",
        ),
        (
            "custom_property_fallback",
            ".card { color: var(--accent, blue); }",
            "function_name",
            "var",
        ),
    ];

    for (name, source, required_kind, required_text) in cases {
        assert!(source.len() <= 8192);
        let tree = parser.parse(source, None).expect("bounded fixture parses");
        assert!(!tree.root_node().has_error(), "{name}");
        assert_eq!(tree.root_node().end_byte(), source.len(), "{name}");
        let nodes = snapshot(tree.root_node());
        let matching = nodes
            .iter()
            .filter(|node| {
                source
                    .get(node.start..node.end)
                    .expect("UTF-8 span is exact");
                node.kind == required_kind
                    && source.get(node.start..node.end) == Some(required_text)
            })
            .count();
        assert_eq!(matching, 1, "{name}");
        if name.ends_with("descendant") || name == "escaped_tag" {
            assert_eq!(
                nodes
                    .iter()
                    .filter(|node| node.kind == "descendant_selector")
                    .count(),
                1,
                "{name}"
            );
        }
        assert!(
            incremental_matches_clean(&mut parser, &tree, source),
            "{name}"
        );
    }
}

#[test]
fn css_invalid_identifier_starts_and_non_css_whitespace_are_not_accepted() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_css::LANGUAGE.into())
        .expect("audited ABI loads");
    for (name, source) in [
        ("invalid_unescaped_digit_class", ".123 { color: red; }"),
        ("invalid_digit_descendant", "main 123 { color: red; }"),
        ("invalid_newline_escape", ".\\\nfoo { color: red; }"),
        ("invalid_form_feed_escape", ".\\\u{c}foo { color: red; }"),
        ("vertical_tab_is_not_whitespace", ".a { \u{b}color: red; }"),
    ] {
        let tree = parser.parse(source, None).expect("bounded negative parses");
        assert!(tree.root_node().has_error(), "{name}");
    }
}

fn incremental_matches_clean(parser: &mut Parser, tree: &Tree, source: &str) -> bool {
    let suffix = "\n.extra { color: blue; }";
    let updated = format!("{source}{suffix}");
    let old_end = end_point(source);
    let mut previous = tree.clone();
    previous.edit(&InputEdit {
        start_byte: source.len(),
        old_end_byte: source.len(),
        new_end_byte: updated.len(),
        start_position: old_end,
        old_end_position: old_end,
        new_end_position: end_point(&updated),
    });
    let incremental = parser
        .parse(&updated, Some(&previous))
        .expect("incremental parse");
    let mut independent = Parser::new();
    independent
        .set_language(&tree_sitter_css::LANGUAGE.into())
        .expect("grammar loads");
    let clean = independent
        .parse(&updated, None)
        .expect("independent clean parse");
    snapshot(incremental.root_node()) == snapshot(clean.root_node())
}

fn end_point(source: &str) -> Point {
    Point {
        row: source.bytes().filter(|byte| *byte == b'\n').count(),
        column: source.rsplit('\n').next().unwrap_or_default().len(),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct NodeSnapshot {
    kind: String,
    start: usize,
    end: usize,
    missing: bool,
    error: bool,
}

fn snapshot(root: Node<'_>) -> Vec<NodeSnapshot> {
    let mut nodes = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        assert!(nodes.len() < 10_000);
        nodes.push(NodeSnapshot {
            kind: node.kind().to_owned(),
            start: node.start_byte(),
            end: node.end_byte(),
            missing: node.is_missing(),
            error: node.is_error(),
        });
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    nodes
}
