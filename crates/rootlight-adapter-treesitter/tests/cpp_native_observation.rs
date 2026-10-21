//! Observe the exact written C++ callback formal and receiver expression.
//! Recovery and preprocessor geometry are recorded without qualifying IR binding.

use tree_sitter::{Node, Parser};

const ORIGINAL: &[u8] = include_bytes!("fixtures/cpp_waforth_formal/main.cpp");

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
        "kind={} field={:?} bytes={}..{} named={} error={} missing={} has_error={}",
        node.kind(),
        field,
        node.start_byte(),
        node.end_byte(),
        node.is_named(),
        node.is_error(),
        node.is_missing(),
        node.has_error()
    )
}

fn owner<'tree>(root: Node<'tree>, start: usize, end: usize, tag: &str) -> Node<'tree> {
    let leaf = root.descendant_for_byte_range(start, end).unwrap();
    assert_eq!((leaf.start_byte(), leaf.end_byte()), (start, end));
    let mut current = Some(leaf);
    for _ in 0..32 {
        let node = current.expect("the exact written anchor has a native owner");
        println!("{tag}-path {}", describe(node));
        if node.kind() == "function_definition" {
            return node;
        }
        current = node.parent();
    }
    panic!("the selected ancestry exceeded its explicit observation bound");
}

fn full_file_census(root: Node<'_>) {
    let mut cursor = root.walk();
    let mut functions = 0;
    let mut recovery = 0;
    for index in 0..10_000 {
        let node = cursor.node();
        if node.kind() == "function_definition" {
            functions += 1;
            println!("file-function {}", describe(node));
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
                    "file-census complete=true nodes={} functions={functions} recovery={recovery}",
                    index + 1
                );
                return;
            }
        }
    }
    panic!("the original file exceeded the complete observation census bound");
}

#[test]
fn original_cpp_const_reference_formal_native_fields_and_exact_owner() {
    assert_eq!(ORIGINAL.len(), 7028);
    assert_eq!(&ORIGINAL[814..821], b"emit_cb");
    assert_eq!(&ORIGINAL[866..872], b"params");
    assert_eq!(&ORIGINAL[935..941], b"params");
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ORIGINAL, None).unwrap();
    let root = tree.root_node();
    println!(
        "source bytes={} expected_sha256=ba5e7082cd6111fbd7a6e2f68ab25b0ca90711655aaa333d1ca77ef43969311c root={}",
        ORIGINAL.len(),
        describe(root)
    );
    assert_eq!(root.kind(), "translation_unit");
    assert_eq!((root.start_byte(), root.end_byte()), (0, ORIGINAL.len()));
    full_file_census(root);
    let function = owner(root, 814, 821, "function");
    assert_eq!(owner(root, 866, 872, "formal"), function);
    assert_eq!(owner(root, 935, 941, "reference"), function);
    println!("selected-function {}", describe(function));
    println!("selected-function-sexp {}", function.to_sexp());
    if let Some(body) = function.child_by_field_name("body") {
        println!("selected-body {}", describe(body));
        println!("selected-body-sexp {}", body.to_sexp());
    }
    // The original retained product report has one recovery gap. Record its
    // actual location instead of assuming it invalidates or spares this owner.
    let mut cursor = function.walk();
    for index in 0..1024 {
        println!("selected-node {index} {}", describe(cursor.node()));
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                println!("selected-census complete=true nodes={}", index + 1);
                return;
            }
        }
    }
    panic!("the selected function exceeded its complete observation census bound");
}
