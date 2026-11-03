//! Native VB.NET controls preserve an original UTF-8 BOM/CRLF source.
//! The tested declaration structure does not qualify body binding semantics.

use tree_sitter::{Node, Parser};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/vbnet/TaskbarAnimate.vb");

fn parse(source: &[u8]) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_vbnet::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
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
fn original_bom_crlf_source_has_exact_native_class_method_and_parameters() {
    assert_eq!(ORIGINAL.len(), 4124);
    assert_eq!(&ORIGINAL[..3], b"\xef\xbb\xbf");
    let tree = parse(ORIGINAL);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let classes = nodes(root, "class_block");
    assert_eq!(classes.len(), 1);
    let class = classes[0];
    let name = class.child_by_field_name("name").unwrap();
    assert_eq!(name.byte_range(), 36..50);
    assert_eq!(&ORIGINAL[name.byte_range()], b"TaskbarAnimate");
    let methods = nodes(class, "method_declaration");
    assert_eq!(methods.len(), 1);
    let method = methods[0];
    assert_eq!(method.parent(), Some(class));
    let name = method.child_by_field_name("name").unwrap();
    assert_eq!(name.byte_range(), 122..129);
    assert_eq!(&ORIGINAL[name.byte_range()], b"Animate");
    let parameters = nodes(method, "parameter");
    assert_eq!(parameters.len(), 8);
    let name = parameters[0].child_by_field_name("name").unwrap();
    assert_eq!(name.byte_range(), 136..140);
    assert_eq!(&ORIGINAL[name.byte_range()], b"hwnd");
}

#[test]
fn quoted_and_commented_names_do_not_create_native_declarations() {
    let source = b"Public Class Owner\nPublic Sub Run(ByVal Value As Integer)\nDim text As String = \"Class Fake\"\n' Public Sub Hidden()\nEnd Sub\nEnd Class\n";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(nodes(root, "class_block").len(), 1);
    assert_eq!(nodes(root, "method_declaration").len(), 1);
    assert_eq!(nodes(root, "parameter").len(), 1);
}

#[test]
fn mixed_case_keywords_parse_and_incomplete_owner_keeps_recovery() {
    let tree =
        parse(b"pUbLiC cLaSs Owner\npUbLiC sUb Run(ByVal Value As Integer)\nEnD sUb\nEnD cLaSs\n");
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let broken = parse(b"Public Class Owner\nPublic Sub Run(ByVal Value As Integer)\n");
    assert!(
        broken.root_node().has_error(),
        "{}",
        broken.root_node().to_sexp()
    );
}
