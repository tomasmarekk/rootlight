//! Observe the complete original Go call source before binding implementation.
//! Native geometry and capture ancestry are evidence, not a semantic qualification.

use tree_sitter::{Node, Parser};

const ORIGINAL: &[u8] = include_bytes!("fixtures/go_written_calls/time.go");

fn describe(node: Node<'_>) -> String {
    let field = node.parent().and_then(|parent| {
        (0..parent.child_count()).find_map(|index| {
            let index = u32::try_from(index).unwrap();
            (parent.child(index) == Some(node))
                .then(|| parent.field_name_for_child(index))
                .flatten()
        })
    });
    format!(
        "id={} kind={} field={field:?} bytes={}..{} error={} missing={}",
        node.id(),
        node.kind(),
        node.start_byte(),
        node.end_byte(),
        node.is_error(),
        node.is_missing()
    )
}

#[test]
fn original_go_time_bare_call_fields_and_complete_native_census() {
    assert_eq!(ORIGINAL.len(), 1478);
    for (start, end, bytes) in [
        (185, 198, b"humanDuration".as_slice()),
        (993, 1002, b"HumanTime"),
        (1051, 1060, b"humanTime"),
        (1083, 1097, b"HumanTimeLower"),
        (1162, 1171, b"humanTime"),
        (1195, 1204, b"humanTime"),
        (1398, 1411, b"humanDuration"),
    ] {
        assert_eq!(&ORIGINAL[start..end], bytes);
    }
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ORIGINAL, None).unwrap();
    println!(
        "source bytes=1478 sha256=8702cb3ab90c455b434d68cc38114660bc1ebaf8c39d74bca1037bebe54bdbc7 root={}",
        describe(tree.root_node())
    );
    println!("full-sexp {}", tree.root_node().to_sexp());
    assert!(!tree.root_node().has_error());
    for (start, end) in [(1051, 1060), (1162, 1171), (1398, 1411)] {
        let mut current = Some(
            tree.root_node()
                .descendant_for_byte_range(start, end)
                .unwrap(),
        );
        for _ in 0..32 {
            let Some(node) = current else { break };
            println!("anchor {start}..{end} {}", describe(node));
            current = node.parent();
        }
    }
    let mut cursor = tree.root_node().walk();
    for index in 0..2048 {
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
    panic!("the explicit original observation bound was exceeded");
}
