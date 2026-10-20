//! Retained Modelica ABI14 tables parse exact source without runtime semantics.

const ORIGINAL: &[u8] =
    include_bytes!("../../../crates/rootlight-adapter-treesitter/tests/fixtures/modelica/U_sf.mo");

#[test]
fn original_and_real_utf8_bom_have_exact_function_and_end_leaf_ranges() {
    for prefix in [b"".as_slice(), b"\xef\xbb\xbf".as_slice()] {
        let raw = [prefix, ORIGINAL].concat();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_modelica::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(&raw, None).unwrap();
        assert!(!tree.root_node().has_error());
        let mut cursor = tree.root_node().walk();
        let mut leaves = Vec::new();
        loop {
            let node = cursor.node();
            if node.kind() == "IDENT" && raw.get(node.byte_range()) == Some(b"U_sf") {
                leaves.push(node.byte_range());
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
                            40 + prefix.len()..44 + prefix.len(),
                            231 + prefix.len()..235 + prefix.len()
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
fn invalid_string_escape_remains_a_real_native_parse_gap() {
    let raw = b"function f annotation(Documentation(info=\"\\</a>\")); end f;";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_modelica::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(raw, None).unwrap();
    assert!(tree.root_node().has_error());
}
