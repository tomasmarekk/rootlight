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

#[test]
fn original_method_implementations_have_native_owners_and_generic_statement_bodies() {
    for (source, class_name, method_name) in [
        (
            include_bytes!("../fixtures/abap/zcl_abapgit_diff_factory.clas.abap").as_slice(),
            b"zcl_abapgit_diff_factory".as_slice(),
            b"get".as_slice(),
        ),
        (
            include_bytes!("../fixtures/abap/zcl_abapgit_object_tabl.clas.testclasses.abap")
                .as_slice(),
            b"ltcl_ci".as_slice(),
            b"run".as_slice(),
        ),
    ] {
        let tree = parser().parse(source, None).unwrap();
        let root = tree.root_node();
        assert!(!root.has_error(), "{}", root.to_sexp());
        let owners = nodes(root, "class_implementation");
        assert_eq!(owners.len(), 1);
        let owner = owners[0];
        assert_eq!(owner.parent(), Some(root));
        assert_eq!(
            &source[owner.child_by_field_name("name").unwrap().byte_range()],
            class_name
        );
        let methods = nodes(owner, "method_implementation");
        assert_eq!(methods.len(), 1);
        let method = methods[0];
        assert_eq!(method.parent(), Some(owner));
        let mut cursor = method.walk();
        let names: Vec<_> = method.children_by_field_name("name", &mut cursor).collect();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].kind(), "identifier");
        assert_eq!(&source[names[0].byte_range()], method_name);
        assert!(method.child_by_field_name("body").is_none());
        let statements = nodes(method, "statement");
        assert_eq!(statements.len(), 1);
        assert_eq!(statements[0].parent(), Some(method));
        assert!(statements[0].child_by_field_name("function").is_none());
    }
}
