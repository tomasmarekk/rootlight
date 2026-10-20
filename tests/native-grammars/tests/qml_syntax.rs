//! The pinned QML parser preserves exact original leaves and real syntax gaps.

const ORIGINAL: &[u8] = include_bytes!(
    "../../../crates/rootlight-adapter-treesitter/tests/fixtures/qml/SettingsUiState.qml"
);

#[test]
fn original_names_keep_distinct_definition_and_call_native_fields() {
    for prefix in [b"".as_slice(), b"\xef\xbb\xbf".as_slice()] {
        let raw = [prefix, ORIGINAL].concat();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_qmljs::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(&raw, None).unwrap();
        assert!(!tree.root_node().has_error());
        let mut cursor = tree.root_node().walk();
        let mut leaves = Vec::new();
        loop {
            let node = cursor.node();
            if matches!(node.kind(), "identifier" | "property_identifier")
                && raw.get(node.byte_range()) == Some(b"normalizeSelectedBar")
            {
                leaves.push((
                    node.kind().to_owned(),
                    node.parent().unwrap().kind().to_owned(),
                    node.byte_range(),
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
                    assert_eq!(
                        leaves,
                        vec![
                            (
                                "identifier".to_owned(),
                                "function_declaration".to_owned(),
                                165 + prefix.len()..185 + prefix.len()
                            ),
                            (
                                "property_identifier".to_owned(),
                                "member_expression".to_owned(),
                                456 + prefix.len()..476 + prefix.len()
                            ),
                        ]
                    );
                    break;
                }
            }
            if cursor.node() == tree.root_node() {
                break;
            }
        }
    }
}

#[test]
fn incomplete_object_remains_a_native_parse_gap() {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_qmljs::LANGUAGE.into())
        .unwrap();
    let tree = parser
        .parse(b"import QtQuick\nItem { function unfinished(", None)
        .unwrap();
    assert!(tree.root_node().has_error());
}
