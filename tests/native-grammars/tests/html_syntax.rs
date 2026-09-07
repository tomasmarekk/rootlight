//! Qualifies HTML syntax before structural adapter registration.
//! Source extents and fresh/incremental parity must survive scanner persistence.

use tree_sitter::{InputEdit, Node, Parser, Point};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_html::LANGUAGE.into())
        .unwrap();
    parser
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![root];
    let mut found = Vec::new();
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

#[test]
fn html_long_custom_names_survive_scanner_serialization() {
    for length in [254, 255, 256, 300, 700] {
        let name = format!("x-{}", "a".repeat(length));
        let source = format!("<{name}><b>text</b></{name}>");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "length {length}: {}",
            tree.root_node().to_sexp()
        );
        let names = nodes(tree.root_node(), "tag_name");
        assert_eq!(names.len(), 4);
        for index in [0, 3] {
            assert_eq!(names[index].utf8_text(source.as_bytes()).unwrap(), name);
        }
    }
}

#[test]
fn html_custom_names_preserve_unicode_and_punctuation() {
    for name in ["x_widget", "x-ž", "x-Ж", "x-💡", "x.name", "ns:element"] {
        let source = format!("<{name}><span>text</span></{name}>");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{name}: {}",
            tree.root_node().to_sexp()
        );
        assert!(nodes(tree.root_node(), "erroneous_end_tag").is_empty());
        for node in nodes(tree.root_node(), "tag_name") {
            assert!([name, "span"].contains(&node.utf8_text(source.as_bytes()).unwrap()));
        }
    }
}

#[test]
fn html_raw_text_requires_a_complete_end_tag_name() {
    for (tag, body) in [
        ("script", "const text = '</scripture>'; next();"),
        ("script", "const text = '<</scriptX>'; next();"),
        ("style", "a::after { content: '</stylesheet>'; }"),
    ] {
        let source = format!("<{tag}>{body}</{tag}>");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let raw = nodes(tree.root_node(), "raw_text");
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].utf8_text(source.as_bytes()).unwrap(), body);
    }
}

#[test]
fn html_case_folding_never_equates_distinct_non_ascii_names() {
    for (source, errors) in [
        ("<X-ž></x-ž>", 0),
        ("<x-ž></x-Ž>", 1),
        ("<x-ž><x-Ж></x-Ж></x-ž>", 0),
        ("<x-first><x-second></x-other>", 1),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert_eq!(
            nodes(tree.root_node(), "erroneous_end_tag").len(),
            errors,
            "{source}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn html_syntax_keeps_duplicate_attributes_void_elements_and_implicit_ends() {
    let source = "<!DOCTYPE html><html><body><ul><li id='a' id=second>one<li>two</ul><input disabled><img src=asset.png /><br><x-widget data-label='💡 &amp; text'></x-widget></body></html>";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(
        nodes(tree.root_node(), "attribute_name")
            .iter()
            .filter(|node| node.utf8_text(source.as_bytes()).unwrap() == "id")
            .count(),
        2
    );
    assert_eq!(nodes(tree.root_node(), "self_closing_tag").len(), 1);
    assert_eq!(nodes(tree.root_node(), "doctype").len(), 1);
}

#[test]
fn html_unserializable_names_are_errors_not_truncated_success() {
    for length in [1019, 1024, 2048] {
        let name = "x".repeat(length);
        let source = format!("<{name}>value</{name}>");
        let mut parser = parser();
        let tree = parser.parse(&source, None).unwrap();
        assert!(tree.root_node().has_error(), "length {length}");
        assert!(
            !parser
                .parse("<div>safe</div>", None)
                .unwrap()
                .root_node()
                .has_error()
        );
    }
}

fn point(source: &str, end: usize) -> Point {
    let prefix = &source[..end];
    Point::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count(),
        end - prefix.rfind('\n').map_or(0, |position| position + 1),
    )
}

fn shape(root: Node<'_>) -> Vec<(String, usize, usize, bool)> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        result.push((
            node.kind().to_owned(),
            node.start_byte(),
            node.end_byte(),
            node.is_missing(),
        ));
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result
}

#[test]
fn html_incremental_edits_match_fresh_trees_and_source_extents() {
    for initial in [
        "<x-ž><span>one</span></x-ž>".to_owned(),
        "<script>let x = '</scripture>';</script><style>a{}</style>".to_owned(),
        "<ul>\r\n<li id='first'>one<li>two</ul>".to_owned(),
        format!("<x-{}><b>one</b></x-{}>", "a".repeat(300), "a".repeat(300)),
    ] {
        let mut source = initial;
        let mut parser = parser();
        let mut tree = parser.parse(&source, None).unwrap();
        for step in 0..40 {
            let boundaries: Vec<_> = source
                .char_indices()
                .map(|(offset, _)| offset)
                .chain([source.len()])
                .collect();
            let index = (step * 37) % boundaries.len();
            let start = boundaries[index];
            let end = if step % 3 == 0 {
                boundaries.get(index + 1).copied().unwrap_or(start)
            } else {
                start
            };
            let text = ["x", "💡", "\n", "", "<", ">"][step % 6];
            let mut changed = source.clone();
            changed.replace_range(start..end, text);
            tree.edit(&InputEdit {
                start_byte: start,
                old_end_byte: end,
                new_end_byte: start + text.len(),
                start_position: point(&source, start),
                old_end_position: point(&source, end),
                new_end_position: point(&changed, start + text.len()),
            });
            let incremental = parser.parse(&changed, Some(&tree)).unwrap();
            let fresh = fresh_tree(&changed);
            assert_eq!(
                shape(incremental.root_node()),
                shape(fresh.root_node()),
                "step {step}: {changed}"
            );
            source = changed;
            tree = incremental;
        }
    }
}

fn fresh_tree(source: &str) -> tree_sitter::Tree {
    parser().parse(source, None).unwrap()
}
