//! Check native physical spans, syntax refusal and edit equivalence.
//! This is parser behavior, independent of production routing and RDF inference.

use tree_sitter::{InputEdit, Parser, Point, Tree};

type Shape = (String, usize, usize, bool, bool, bool);

fn parser() -> Parser {
    let language: tree_sitter::Language = tree_sitter_turtle::LANGUAGE.into();
    assert_eq!(language.abi_version(), 14);
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("retained ABI14 loads");
    parser
}

fn shape(tree: &Tree) -> Vec<Shape> {
    let mut result = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        result.push((
            node.kind().to_owned(),
            node.start_byte(),
            node.end_byte(),
            node.is_named(),
            node.is_error(),
            node.is_missing(),
        ));
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

fn clean(source: &[u8], tree: &Tree) {
    let root = tree.root_node();
    assert_eq!(root.kind(), "document");
    assert_eq!(root.start_byte(), 0);
    assert_eq!(root.end_byte(), source.len());
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert!(shape(tree).iter().all(|node| !node.4 && !node.5));
}

#[test]
fn retained_turtle_parser_preserves_spans_errors_and_edits() {
    let source = "@prefix ex: <https://example.test/> .\r\nex:s ex:p \"λ😀\" .\r\n".as_bytes();
    let tree = parser().parse(source, None).expect("native parse");
    clean(source, &tree);
    let nodes = shape(&tree);
    assert_eq!(nodes.iter().filter(|node| node.0 == "prefix_id").count(), 1);
    assert_eq!(nodes.iter().filter(|node| node.0 == "triple").count(), 1);
    let literal = nodes.iter().find(|node| node.0 == "rdf_literal").unwrap();
    assert_eq!(&source[literal.1..literal.2], "\"λ😀\"".as_bytes());

    let start = source
        .windows(4)
        .position(|bytes| bytes == b"ex:s")
        .unwrap()
        + 3;
    let mut changed = source.to_vec();
    changed[start] = b't';
    let row = source[..start]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();
    let column = start
        - source[..start]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1);
    let mut edited = tree.clone();
    edited.edit(&InputEdit {
        start_byte: start,
        old_end_byte: start + 1,
        new_end_byte: start + 1,
        start_position: Point::new(row, column),
        old_end_position: Point::new(row, column + 1),
        new_end_position: Point::new(row, column + 1),
    });
    let incremental = parser()
        .parse(&changed, Some(&edited))
        .expect("incremental parse");
    let fresh = parser().parse(&changed, None).expect("fresh changed parse");
    clean(&changed, &incremental);
    clean(&changed, &fresh);
    assert_eq!(shape(&incremental), shape(&fresh));

    for invalid in [
        b"@prefix ex: <https://example.test/> .\nex:s ex:p .\n".as_slice(),
        b"@prefix ex: <https://example.test/> .\n{ ex:s ex:p ex:o . } => { ex:s ex:q ex:o . } .\n"
            .as_slice(),
    ] {
        let refused = parser().parse(invalid, None).expect("native recovery tree");
        assert!(
            refused.root_node().has_error(),
            "{}",
            refused.root_node().to_sexp()
        );
    }
}

#[test]
fn trig_graph_keyword_is_case_insensitive_with_named_native_labels() {
    let prefix = "@prefix ex: <https://example.test/> .\n";
    for keyword in ["GRAPH", "graph", "GrApH"] {
        let source = format!("{prefix}{keyword} ex:g {{ ex:s ex:p ex:o }}\n");
        let tree = parser().parse(&source, None).unwrap();
        clean(source.as_bytes(), &tree);
        let mut cursor = tree.root_node().walk();
        let graph = tree
            .root_node()
            .named_children(&mut cursor)
            .find(|node| node.kind() == "graph")
            .unwrap();
        let mut labels = graph.walk();
        let label = graph
            .children_by_field_name("label", &mut labels)
            .find(|node| node.is_named())
            .unwrap();
        assert_eq!(label.kind(), "prefixed_name");
        assert_eq!(label.utf8_text(source.as_bytes()).unwrap(), "ex:g");
    }
    let source = format!("{prefix}GRAPH ex:g {{ ex:s ex:p ex:o }}\n");
    let mut edited = parser().parse(&source, None).unwrap();
    let mut changed = source.into_bytes();
    let start = prefix.len();
    changed[start..start + 5].copy_from_slice(b"graph");
    edited.edit(&InputEdit {
        start_byte: start,
        old_end_byte: start + 5,
        new_end_byte: start + 5,
        start_position: Point::new(1, 0),
        old_end_position: Point::new(1, 5),
        new_end_position: Point::new(1, 5),
    });
    let incremental = parser().parse(&changed, Some(&edited)).unwrap();
    let fresh = parser().parse(&changed, None).unwrap();
    clean(&changed, &incremental);
    clean(&changed, &fresh);
    assert_eq!(shape(&incremental), shape(&fresh));
}

#[test]
fn trig_graph_keyword_requires_a_label_and_does_not_accept_longer_keywords() {
    for source in [
        b"GRAPH { }".as_slice(),
        b"graph { }".as_slice(),
        b"GRAPHIC <https://example.test/g> { }".as_slice(),
    ] {
        assert!(
            parser()
                .parse(source, None)
                .unwrap()
                .root_node()
                .has_error()
        );
    }
}
