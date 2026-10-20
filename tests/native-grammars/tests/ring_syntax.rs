//! Native Ring ownership remains a separate prerequisite to product facts.
//! A clean multi-class CST is deliberately not treated as source ownership proof.

const ORIGINAL: &[u8] = include_bytes!(
    "../../../crates/rootlight-adapter-treesitter/tests/fixtures/ring/htmlfunctions.ring"
);

#[test]
fn original_native_tree_has_exact_class_method_formal_and_required_read() {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_ring::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ORIGINAL, None).unwrap();
    assert!(!tree.root_node().has_error());
    let mut found = Vec::new();
    let mut cursor = tree.root_node().walk();
    loop {
        let node = cursor.node();
        if node.kind() == "identifier" && [209, 383, 400, 439].contains(&node.start_byte()) {
            found.push((
                node.start_byte(),
                node.end_byte(),
                node.parent().unwrap().kind().to_owned(),
            ));
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break;
            }
        }
        if cursor.node() == tree.root_node() {
            break;
        }
    }
    assert_eq!(
        found,
        vec![
            (209, 222, "class_definition".to_owned()),
            (383, 399, "function_definition".to_owned()),
            (400, 404, "param_list".to_owned()),
            (439, 443, "arguments".to_owned())
        ]
    );
}

#[test]
fn malformed_header_stays_error_and_multiple_class_nesting_is_not_semantic_proof() {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_ring::LANGUAGE.into())
        .unwrap();
    assert!(
        parser
            .parse(
                b"class Written\nfunc measure(value, { return value }\n",
                None
            )
            .unwrap()
            .root_node()
            .has_error()
    );
    let tree = parser.parse(b"class First\nfunc first value { return value }\nclass Second\nfunc second value { return value }\n", None).unwrap();
    assert!(!tree.root_node().has_error());
    let first = tree.root_node().named_child(0).unwrap();
    assert_eq!(first.kind(), "class_definition");
    let mut cursor = first.walk();
    assert!(
        first
            .named_children(&mut cursor)
            .any(|node| node.kind() == "class_definition")
    );
}
