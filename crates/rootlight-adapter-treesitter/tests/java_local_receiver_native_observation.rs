//! Observe a written Java local and its receiver use without resolving a call.

use tree_sitter::{Node, Parser};

const ORIGINAL: &[u8] = include_bytes!("fixtures/java_local_receiver/TestPipelineJava.java");

fn describe(node: Node<'_>) -> String {
    let mut field = None;
    if let Some(parent) = node.parent() {
        for index in 0..parent.child_count() {
            let index = u32::try_from(index).unwrap();
            if parent.child(index) == Some(node) {
                field = parent.field_name_for_child(index);
                break;
            }
        }
    }
    format!(
        "kind={} field={field:?} bytes={}..{} named={} error={} missing={} has_error={}",
        node.kind(),
        node.start_byte(),
        node.end_byte(),
        node.is_named(),
        node.is_error(),
        node.is_missing(),
        node.has_error()
    )
}

fn ancestor<'tree>(
    root: Node<'tree>,
    start: usize,
    end: usize,
    kind: &str,
    label: &str,
) -> Node<'tree> {
    let leaf = root.descendant_for_byte_range(start, end).unwrap();
    assert_eq!((leaf.start_byte(), leaf.end_byte()), (start, end));
    let mut current = Some(leaf);
    for _ in 0..32 {
        let node = current.expect("the exact physical anchor has the required native owner");
        println!("{label}-ancestry {}", describe(node));
        if node.kind() == kind {
            return node;
        }
        current = node.parent();
    }
    panic!("the selected native ancestry exceeded its explicit observation bound");
}

fn file_census(root: Node<'_>) {
    let mut cursor = root.walk();
    let mut recovery = 0;
    for index in 0..10_000 {
        let node = cursor.node();
        if matches!(node.kind(), "class_declaration" | "method_declaration") {
            println!("file-owner {}", describe(node));
        }
        if node.is_error() || node.is_missing() {
            recovery += 1;
            println!("file-recovery {}", describe(node));
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                println!(
                    "file-census complete=true nodes={} recovery={recovery}",
                    index + 1
                );
                return;
            }
        }
    }
    panic!("the complete original file exceeded the explicit observation bound");
}

#[test]
fn original_java_local_receiver_native_fields_and_exact_method_owner() {
    assert_eq!(ORIGINAL.len(), 1209);
    assert_eq!(&ORIGINAL[549..565], b"TestPipelineJava");
    assert_eq!(&ORIGINAL[975..999], b"should_return_clean_name");
    assert_eq!(&ORIGINAL[1020..1026], b"script");
    assert_eq!(&ORIGINAL[1082..1088], b"script");
    assert_eq!(
        ORIGINAL.windows(2).filter(|pair| *pair == b"\r\n").count(),
        36
    );
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ORIGINAL, None).unwrap();
    let root = tree.root_node();
    println!(
        "source bytes={} expected_physical_sha256=dbccbee480ecd046bd32e8400710a5525f4c31d5c9693b7be882e7b1fc034aad root={}",
        ORIGINAL.len(),
        describe(root)
    );
    assert_eq!(root.kind(), "program");
    assert_eq!((root.start_byte(), root.end_byte()), (0, ORIGINAL.len()));
    file_census(root);
    let class = ancestor(root, 549, 565, "class_declaration", "class-name");
    let method = ancestor(root, 975, 999, "method_declaration", "method-name");
    let local_method = ancestor(root, 1020, 1026, "method_declaration", "local-name");
    let receiver_method = ancestor(root, 1082, 1088, "method_declaration", "receiver-name");
    println!("selected-method {}", describe(method));
    println!("selected-method-sexp {}", method.to_sexp());
    assert_eq!((class.start_byte(), class.end_byte()), (543, 1207));
    assert_eq!((method.start_byte(), method.end_byte()), (959, 1204));
    assert_eq!(local_method, method);
    assert_eq!(receiver_method, method);
    assert_eq!(
        ancestor(root, 975, 999, "class_declaration", "method-class"),
        class
    );
    let body = method
        .child_by_field_name("body")
        .expect("written method body");
    assert_eq!(body.kind(), "block");
    assert_eq!((body.start_byte(), body.end_byte()), (1002, 1204));
    println!("selected-body {}", describe(body));
    println!("selected-body-sexp {}", body.to_sexp());
    let local = root.descendant_for_byte_range(1020, 1026).unwrap();
    let declarator = local.parent().expect("local native declarator");
    assert_eq!(declarator.kind(), "variable_declarator");
    assert_eq!(declarator.child_by_field_name("name"), Some(local));
    assert_eq!(
        (declarator.start_byte(), declarator.end_byte()),
        (1020, 1060)
    );
    let declaration = declarator.parent().expect("written local declaration");
    assert_eq!(declaration.kind(), "local_variable_declaration");
    assert_eq!(
        (declaration.start_byte(), declaration.end_byte()),
        (1013, 1061)
    );
    assert_eq!(declaration.parent(), Some(body));
    let receiver = root.descendant_for_byte_range(1082, 1088).unwrap();
    let invocation = receiver.parent().expect("receiver's native invocation");
    assert_eq!(invocation.kind(), "method_invocation");
    assert_eq!(invocation.child_by_field_name("object"), Some(receiver));
    println!("receiver-invocation {}", describe(invocation));
    let mut cursor = method.walk();
    for index in 0..1024 {
        let node = cursor.node();
        println!("selected-node {index} {}", describe(node));
        assert!(
            !node.is_error() && !node.is_missing(),
            "selected method is native clean"
        );
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                println!("selected-census complete=true nodes={}", index + 1);
                assert!(!method.has_error());
                return;
            }
        }
    }
    panic!("the selected method exceeded the complete observation census bound");
}
