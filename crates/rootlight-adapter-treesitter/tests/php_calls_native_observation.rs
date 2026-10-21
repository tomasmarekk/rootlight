//! Observe the full licensed PHP member-call source before static binding.
//! Native fields and ancestry do not establish a unique runtime implementation.

use tree_sitter::{Node, Parser, StreamingIterator};

const ORIGINAL: &[u8] = include_bytes!("fixtures/php_constructors/HtmlString.php");

fn describe(node: Node<'_>) -> String {
    let field = node.parent().and_then(|parent| {
        (0..parent.child_count()).find_map(|index| {
            let index = u32::try_from(index).unwrap();
            (parent.child(index) == Some(node))
                .then(|| parent.field_name_for_child(index))
                .flatten()
        })
    });
    format!(
        "id={} kind={} field={field:?} bytes={}..{} error={} missing={}",
        node.id(),
        node.kind(),
        node.start_byte(),
        node.end_byte(),
        node.is_error(),
        node.is_missing()
    )
}

fn observe(source: &[u8], anchors: &[(usize, usize)]) {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    println!(
        "source-bytes={} root={}",
        source.len(),
        describe(tree.root_node())
    );
    println!("full-sexp {}", tree.root_node().to_sexp());
    assert!(!tree.root_node().has_error());
    for &(start, end) in anchors {
        let mut node = Some(
            tree.root_node()
                .descendant_for_byte_range(start, end)
                .unwrap(),
        );
        for _ in 0..32 {
            let Some(current) = node else { break };
            println!("anchor {start}..{end} {}", describe(current));
            node = current.parent();
        }
    }
    let language = tree_sitter_php::LANGUAGE_PHP.into();
    let query = tree_sitter::Query::new(&language,
        "(class_declaration name: (name) @class) (method_declaration name: (name) @method) (member_call_expression object: (_) @receiver name: (_) @member) @call").unwrap();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source);
    let mut count = 0;
    while let Some((matched, index)) = captures.next() {
        count += 1;
        assert!(count <= 256, "bounded observation capture census");
        let capture = matched.captures[*index];
        println!(
            "capture {} {}",
            query.capture_names()[usize::try_from(capture.index).unwrap()],
            describe(capture.node)
        );
    }
    let mut cursor = tree.root_node().walk();
    for index in 0..2048 {
        println!("node {index} {}", describe(cursor.node()));
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
    panic!("the finite original node observation bound was exceeded");
}

#[test]
fn original_php_member_call_fields_and_complete_ancestry() {
    assert_eq!(ORIGINAL.len(), 1145);
    for (start, end, expected) in [
        (111, 121, b"HtmlString".as_slice()),
        (541, 547, b"toHtml"),
        (1075, 1085, b"__toString"),
        (1118, 1124, b"toHtml"),
    ] {
        assert_eq!(&ORIGINAL[start..end], expected);
    }
    println!(
        "expected-source-pin=59d333c0de421196cb51489a3f43202cf569186f4cae6e92eca0da034cc79fb0"
    );
    observe(
        ORIGINAL,
        &[(111, 121), (541, 547), (1075, 1085), (1118, 1124)],
    );
    let ordinary = b"<?php class Base { public function target() {} public function caller() { return $this->TARGET(); } } class Child extends Base { public function target() {} }";
    let site = ordinary
        .windows(b"TARGET".len())
        .position(|bytes| bytes == b"TARGET")
        .unwrap();
    observe(ordinary, &[(site, site + 6)]);
}
