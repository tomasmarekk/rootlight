//! Qualifies complete component identities across scanner snapshots and edits.
//! Parser trees are compared with a fresh parse, including every source range.

use tree_sitter::{InputEdit, Node, Parser, Point};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_astro_next::LANGUAGE.into())
        .unwrap();
    parser
}

fn signature(node: Node<'_>, source: &str, result: &mut Vec<(String, usize, usize, String)>) {
    result.push((
        node.kind().to_owned(),
        node.start_byte(),
        node.end_byte(),
        node.utf8_text(source.as_bytes()).unwrap().to_owned(),
    ));
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        signature(child, source, result);
    }
}

#[test]
fn complete_component_names_survive_snapshot_boundaries() {
    for length in [1, 254, 255, 256, 500, 900] {
        let name = format!("C{}", "x".repeat(length));
        let source = format!("<{name}><span>before</span></{name}>");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "length {length}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn unicode_component_names_retain_identity() {
    for name in ["Card", "UI.Card", "X-é", "X-ǩ", "X-Ж", "X-💡"] {
        let source = format!("<{name}><span>before</span></{name}>");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{name}: {}",
            tree.root_node().to_sexp()
        );
        let mut nodes = Vec::new();
        signature(tree.root_node(), &source, &mut nodes);
        let names: Vec<_> = nodes
            .iter()
            .filter(|node| node.0 == "tag_name")
            .map(|node| node.3.as_str())
            .collect();
        assert_eq!(names, [name, "span", "span", name]);
    }
}

#[test]
fn incremental_component_tree_equals_fresh_source_ranges() {
    for name in [
        "Card".to_owned(),
        format!("C{}", "x".repeat(300)),
        "X-é".to_owned(),
    ] {
        let old = format!("<{name}><span>before</span></{name}>");
        let new = old.replace("before", "after");
        let start = old.find("before").unwrap();
        let mut parser = parser();
        let mut tree = parser.parse(&old, None).unwrap();
        tree.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + 6,
            new_end_byte: start + 5,
            start_position: Point::new(0, start),
            old_end_position: Point::new(0, start + 6),
            new_end_position: Point::new(0, start + 5),
        });
        let incremental = parser.parse(&new, Some(&tree)).unwrap();
        let fresh = parser.parse(&new, None).unwrap();
        assert!(!fresh.root_node().has_error(), "{name}");
        let mut left = Vec::new();
        let mut right = Vec::new();
        signature(incremental.root_node(), &new, &mut left);
        signature(fresh.root_node(), &new, &mut right);
        assert_eq!(left, right, "{name}");
    }
}

#[test]
fn repeated_fragments_and_interpolations_release_state_capacity() {
    let source = "<>{ready && <Card><span>value</span></Card>}</>".repeat(1100);
    let tree = parser().parse(&source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn oversized_component_name_is_not_silently_truncated() {
    let name = format!("C{}", "x".repeat(1018));
    let source = format!("<{name}>body</{name}>");
    let tree = parser().parse(&source, None).unwrap();
    assert!(tree.root_node().has_error());
}

#[test]
fn component_names_do_not_alias_by_case_or_truncated_unicode() {
    for (open, close) in [("Card", "card"), ("X-é", "X-ǩ"), ("UI.Card", "UI.card")] {
        let source = format!("<{open}><span>body</span></{close}>");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{open} must not close as {close}"
        );
    }
}
