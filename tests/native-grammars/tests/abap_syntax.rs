//! Native ABAP structure controls use an exact original corpus interface.
//! Clean permissive statements do not establish semantic or corpus coverage.

use tree_sitter::{Node, Parser};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/abap/zif_abapgit_gui_renderable.intf.abap");

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_abap::LANGUAGE.into())
        .expect("retained ABI15 ABAP grammar loads");
    parser
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut result = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            result.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    result.sort_by_key(Node::start_byte);
    result
}

#[test]
fn original_interface_has_exact_native_owner_and_method_names() {
    assert_eq!(ORIGINAL.len(), 186);
    let tree = parser().parse(ORIGINAL, None).unwrap();
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(root.kind(), "source_file");
    assert_eq!(root.byte_range(), 0..ORIGINAL.len());
    let interfaces = nodes(root, "interface_definition");
    assert_eq!(interfaces.len(), 1);
    let interface = interfaces[0];
    let name = interface.child_by_field_name("name").unwrap();
    assert_eq!(name.byte_range(), 10..36);
    assert_eq!(&ORIGINAL[name.byte_range()], b"zif_abapgit_gui_renderable");
    let methods = nodes(interface, "method_entry");
    assert_eq!(methods.len(), 1);
    let method = methods[0];
    let name = method.child_by_field_name("name").unwrap();
    assert_eq!(name.byte_range(), 59..65);
    assert_eq!(&ORIGINAL[name.byte_range()], b"render");
    assert_eq!(method.parent().unwrap().kind(), "methods_declaration");
    assert_eq!(method.parent().unwrap().parent(), Some(interface));
}

#[test]
fn comments_strings_and_multiplication_do_not_create_native_owners() {
    let source = b"* INTERFACE hidden.\n\" METHODS hidden.\nWRITE 'CLASS hidden DEFINITION.'.\nDATA product TYPE i.\nproduct = 2 * 3.\n";
    let tree = parser().parse(source, None).unwrap();
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(nodes(root, "star_comment").len(), 1);
    assert_eq!(nodes(root, "line_comment").len(), 1);
    assert_eq!(nodes(root, "string").len(), 1);
    assert_eq!(nodes(root, "statement").len(), 2);
    for kind in ["interface_definition", "class_definition", "method_entry"] {
        assert!(nodes(root, kind).is_empty(), "unexpected {kind}");
    }
}

#[test]
fn unmodeled_statement_stays_generic_and_incomplete_owner_keeps_error() {
    let source = b"frobnicate missing_target( value ).";
    let tree = parser().parse(source, None).unwrap();
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let statements = nodes(root, "statement");
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0].byte_range(), 0..source.len());
    assert!(statements[0].child_by_field_name("name").is_none());
    let incomplete = parser()
        .parse(b"INTERFACE zif_missing.\n METHODS render.\n", None)
        .unwrap();
    assert!(
        incomplete.root_node().has_error(),
        "{}",
        incomplete.root_node().to_sexp()
    );
}
