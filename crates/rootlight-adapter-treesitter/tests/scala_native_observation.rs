//! Observe the pinned Scala function and formal through the existing native grammar.
//! This exposes source-owned CST evidence only; it does not qualify IR binding.

use tree_sitter::{Node, Parser};

const ORIGINAL: &[u8] = include_bytes!("fixtures/scala_formal/endpoint.scala");

fn describe(node: Node<'_>) -> String {
    let mut field = None;
    if let Some(parent) = node.parent() {
        for index in 0..parent.child_count() {
            if parent.child(u32::try_from(index).unwrap()) == Some(node) {
                field = parent.field_name_for_child(u32::try_from(index).unwrap());
                break;
            }
        }
    }
    format!(
        "kind={} field={:?} bytes={}..{} error={} missing={} has_error={}",
        node.kind(),
        field,
        node.start_byte(),
        node.end_byte(),
        node.is_error(),
        node.is_missing(),
        node.has_error()
    )
}

#[test]
fn original_scala_socket_native_fields_and_ancestry() {
    assert_eq!(ORIGINAL.len(), 3194);
    assert_eq!(&ORIGINAL[1386..1390], b"name");
    assert_eq!(&ORIGINAL[1425..1429], b"name");
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_scala::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ORIGINAL, None).unwrap();
    println!(
        "source bytes={} sha256=8fd63ee198caea59a5ec46b04a6d7c77ac73550759ea2a89ec00c79f5bd4ebdf root={}",
        ORIGINAL.len(),
        describe(tree.root_node())
    );
    let leaf = tree
        .root_node()
        .descendant_for_byte_range(1386, 1390)
        .unwrap();
    let mut current = Some(leaf);
    let mut function = None;
    for _ in 0..32 {
        let Some(node) = current else { break };
        println!("formal-path {}", describe(node));
        if node.kind() == "function_definition" {
            function = Some(node);
            break;
        }
        current = node.parent();
    }
    let function = function.expect("the original formal has a native function owner");
    assert!(!function.has_error() && !function.is_missing());
    println!("function-sexp {}", function.to_sexp());
    let body = function.child_by_field_name("body").unwrap();
    println!("body-sexp {}", body.to_sexp());
    let mut cursor = function.walk();
    for index in 0..200 {
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
    panic!("selected native function exceeded the explicit observation bound");
}
