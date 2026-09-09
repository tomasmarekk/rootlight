//! Pins lexical ownership across nested JavaScript blocks and Astro components.
//! Fresh and incremental trees must preserve all original byte ranges and node kinds.

use tree_sitter::{InputEdit, Node, Parser, Point};

fn signature(node: Node<'_>, source: &str) -> Vec<(String, usize, usize, String)> {
    let mut result = Vec::new();
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        result.push((
            node.kind().to_owned(),
            node.start_byte(),
            node.end_byte(),
            node.utf8_text(source.as_bytes()).unwrap().to_owned(),
        ));
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result
}

#[test]
fn nested_interpolation_contexts_preserve_source_and_incremental_identity() {
    for expression in [
        "/}/.test(value)",
        "/[/*}]/.test(value)",
        "({value: 1}) / divisor",
        "<Card value={value} /> / divisor",
        "<Card>{/}/.test(value)}</Card> / divisor",
        "<><Card value={value} /></> / divisor",
        "(() => { if (value) {} /}/.test(text); return total / count; })()",
        "(() => { function work() {} /}/.test(value); return total / count; })()",
    ] {
        let source = format!("<p>{{{expression}}}</p>");
        let edited = source.replacen("value", "input", 1);
        let position = source.find("value").unwrap();
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_astro_next::LANGUAGE.into())
            .unwrap();
        let mut old = parser.parse(&source, None).unwrap();
        assert!(
            !old.root_node().has_error(),
            "{source}: {}",
            old.root_node().to_sexp()
        );
        assert!(signature(old.root_node(), &source).iter().any(|node| node.0 == "html_interpolation" && node.3 == format!("{{{expression}}}")));
        old.edit(&InputEdit {
            start_byte: position,
            old_end_byte: position + 5,
            new_end_byte: position + 5,
            start_position: Point::new(0, position),
            old_end_position: Point::new(0, position + 5),
            new_end_position: Point::new(0, position + 5),
        });
        let incremental = parser.parse(&edited, Some(&old)).unwrap();
        let fresh = parser.parse(&edited, None).unwrap();
        assert!(!fresh.root_node().has_error(), "{edited}");
        assert_eq!(
            signature(incremental.root_node(), &edited),
            signature(fresh.root_node(), &edited),
            "{source}"
        );
    }
}
