//! Observe complete ENV key fields and owners before written-variable admission.

use tree_sitter::{Node, Parser, StreamingIterator};

const ORIGINAL: &[u8] = include_bytes!("fixtures/dockerfile_env/original.Dockerfile");

fn observe(source: &[u8], expected: &[(&[u8], usize, usize)]) {
    let scope = tree_sitter_dockerfile::ScannerScope::begin((), |_| false).unwrap();
    let mut parser = Parser::new();
    let language = tree_sitter_dockerfile::LANGUAGE.into();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();
    println!(
        "full-source-bytes={} root={}..{} clean={} sexp={}",
        source.len(),
        root.start_byte(),
        root.end_byte(),
        !root.has_error(),
        root.to_sexp()
    );
    assert!(!root.has_error() && !root.is_missing());
    assert_eq!(root.end_byte(), source.len());
    let query = tree_sitter::Query::new(
        &language,
        "(env_instruction) @instruction (env_pair name: (unquoted_string) @name) @pair",
    )
    .unwrap();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut captures = cursor.captures(&query, root, source);
    let mut count = 0;
    while let Some((matched, index)) = captures.next() {
        count += 1;
        assert!(count <= 128);
        let capture = matched.captures[*index];
        println!(
            "capture {} id={} kind={} span={}..{} parent={:?}",
            query.capture_names()[capture.index as usize],
            capture.node.id(),
            capture.node.kind(),
            capture.node.start_byte(),
            capture.node.end_byte(),
            capture
                .node
                .parent()
                .map(|n| (n.id(), n.kind(), n.byte_range()))
        );
    }
    for &(name, start, end) in expected {
        assert_eq!(&source[start..end], name);
        let node = root.descendant_for_byte_range(start, end).unwrap();
        assert_eq!(node.kind(), "unquoted_string");
        assert_eq!(node.byte_range(), start..end);
        let pair = node.parent().unwrap();
        assert_eq!(pair.kind(), "env_pair");
        assert_eq!(pair.child_by_field_name("name"), Some(node));
        let owner = pair.parent().unwrap();
        assert_eq!(owner.kind(), "env_instruction");
        assert_eq!(owner.parent().unwrap().kind(), "source_file");
        let mut ancestry = Some(node);
        while let Some(current) = ancestry {
            println!(
                "anchor {start}..{end} id={} kind={} span={:?}",
                current.id(),
                current.kind(),
                current.byte_range()
            );
            ancestry = current.parent();
        }
    }
    fn visit(node: Node<'_>, count: &mut usize) {
        *count += 1;
        assert!(*count <= 2048);
        println!(
            "node {} id={} kind={} span={}..{} parent={:?} error={} missing={}",
            count,
            node.id(),
            node.kind(),
            node.start_byte(),
            node.end_byte(),
            node.parent().map(|n| n.id()),
            node.is_error(),
            node.is_missing()
        );
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(child, count);
        }
    }
    visit(root, &mut 0);
    drop(parser);
    scope.finish().unwrap();
}

#[test]
fn complete_original_env_fields_and_native_owners() {
    assert_eq!(ORIGINAL.len(), 475);
    observe(ORIGINAL, &[(b"HOST", 206, 210), (b"PORT", 224, 228)]);
}

#[test]
fn ordinary_multiple_empty_and_legacy_env_fields_are_distinct_native_pairs() {
    let source = b"FROM scratch\nENV Alpha=one Beta2=\"two words\" empty=\nENV legacy old\n";
    let text = std::str::from_utf8(source).unwrap();
    let expected: Vec<_> = ["Alpha", "Beta2", "empty", "legacy"]
        .into_iter()
        .map(|name| {
            let start = text.find(name).unwrap();
            (name.as_bytes(), start, start + name.len())
        })
        .collect();
    observe(source, &expected);
}
