//! Original source inputs and nontrivial edits qualify the native component only.

use tree_sitter::{InputEdit, Parser, Point, Tree};

const SOURCES: &[(&str, &[u8])] = &[
    ("eepers.adb", include_bytes!("fixtures/eepers.adb")),
    ("raylib.adb", include_bytes!("fixtures/raylib.adb")),
    ("raylib.ads", include_bytes!("fixtures/raylib.ads")),
    ("raymath.ads", include_bytes!("fixtures/raymath.ads")),
    ("test.adb", include_bytes!("fixtures/test.adb")),
];

type NodeShape = (String, usize, usize, bool, bool, bool, Option<String>);

fn shape(tree: &Tree) -> Vec<NodeShape> {
    let mut cursor = tree.walk();
    let mut output = Vec::new();
    loop {
        let node = cursor.node();
        output.push((
            node.kind().to_owned(),
            node.start_byte(),
            node.end_byte(),
            node.is_named(),
            node.is_missing(),
            node.is_error(),
            cursor.field_name().map(str::to_owned),
        ));
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return output;
            }
        }
    }
}

#[test]
fn all_original_ada_sources_parse_without_recovery_at_the_pinned_abi() {
    let language = tree_sitter_ada::LANGUAGE.into();
    assert_eq!(tree_sitter::Language::abi_version(&language), 15);
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    for (name, source) in SOURCES {
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error(), "{name}");
        assert_eq!(tree.root_node().end_byte(), source.len(), "{name}");
    }
}

#[test]
fn comment_insertion_and_restoration_match_all_fresh_node_coordinates() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_ada::LANGUAGE.into())
        .unwrap();
    let prefix = b"-- edit\n";
    for (name, source) in SOURCES {
        let mut original = parser.parse(source, None).unwrap();
        let mut changed = prefix.to_vec();
        changed.extend_from_slice(source);
        original.edit(&InputEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: prefix.len(),
            start_position: Point::new(0, 0),
            old_end_position: Point::new(0, 0),
            new_end_position: Point::new(1, 0),
        });
        let mut incremental = parser.parse(&changed, Some(&original)).unwrap();
        let fresh = parser.parse(&changed, None).unwrap();
        assert!(!fresh.root_node().has_error(), "{name}");
        assert_eq!(shape(&incremental), shape(&fresh), "{name}");
        incremental.edit(&InputEdit {
            start_byte: 0,
            old_end_byte: prefix.len(),
            new_end_byte: 0,
            start_position: Point::new(0, 0),
            old_end_position: Point::new(1, 0),
            new_end_position: Point::new(0, 0),
        });
        let restored = parser.parse(source, Some(&incremental)).unwrap();
        let fresh_original = parser.parse(source, None).unwrap();
        assert_eq!(shape(&restored), shape(&fresh_original), "{name}");
    }
}
