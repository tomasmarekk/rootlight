//! Observe the complete original OCaml file with the existing native factory.
//! This reports CST ownership only; it cannot qualify resolved IR or language coverage.

use tree_sitter::{Node, Parser};

const ORIGINAL: &[u8] = include_bytes!("fixtures/ocaml_written/caml_bytestring.ml");

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

#[test]
fn original_ocaml_bytestring_native_fields_and_ancestry() {
    assert_eq!(ORIGINAL.len(), 899);
    assert_eq!(&ORIGINAL[37..63], b"caml_bytestring_length_aux");
    assert_eq!(&ORIGINAL[64..65], b"s");
    assert_eq!(&ORIGINAL[81..82], b"s");
    assert_eq!(&ORIGINAL[217..239], b"caml_bytestring_length");
    assert_eq!(&ORIGINAL[244..270], b"caml_bytestring_length_aux");
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_ocaml::LANGUAGE_OCAML.into())
        .unwrap();
    let tree = parser.parse(ORIGINAL, None).unwrap();
    let root = tree.root_node();
    println!("original-bytes={} root {}", ORIGINAL.len(), describe(root));
    println!("included-ranges {:?}", tree.included_ranges());
    println!("complete-sexp {}", root.to_sexp());

    for (label, start, end) in [
        ("helper-definition", 37, 63),
        ("formal-definition", 64, 65),
        ("required-reference", 81, 82),
        ("wrapper-definition", 217, 239),
        ("required-callee", 244, 270),
    ] {
        let mut current = Some(root.descendant_for_byte_range(start, end).unwrap());
        let mut reached_root = false;
        for depth in 0..=ORIGINAL.len() {
            let Some(node) = current else { break };
            println!("anchor={label} depth={depth} {}", describe(node));
            if node.parent().is_none() {
                reached_root = true;
                break;
            }
            current = node.parent();
        }
        assert!(
            reached_root,
            "the bounded ancestry must reach the native file root"
        );
    }

    let maximum_nodes = ORIGINAL.len().checked_mul(8).unwrap();
    let mut cursor = root.walk();
    let mut errors = 0_usize;
    let mut missing = 0_usize;
    let mut complete = false;
    let mut seen = 0_usize;
    for index in 0..maximum_nodes {
        let node = cursor.node();
        errors += usize::from(node.is_error());
        missing += usize::from(node.is_missing());
        seen += 1;
        println!("node={index} depth={} {}", node_depth(node), describe(node));
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                complete = true;
                break;
            }
        }
        if complete {
            break;
        }
    }
    println!("complete={complete} nodes={seen} errors={errors} missing={missing}");
    assert!(
        complete,
        "the observation must print every native node without truncation"
    );
    assert!(
        !root.has_error() && errors == 0 && missing == 0,
        "the original must have a clean native CST"
    );
}

fn node_depth(mut node: Node<'_>) -> usize {
    for depth in 0..=ORIGINAL.len() {
        let Some(parent) = node.parent() else {
            return depth;
        };
        node = parent;
    }
    panic!("the node ancestry exceeded the explicit source-sized bound");
}
