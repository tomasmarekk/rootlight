//! Exercise the retained RON parser on authored syntax and optional corpus bytes.
//! Native parsing and edit equivalence do not qualify production IR or schemas.

use tree_sitter::{InputEdit, Parser, Point, Tree};

type Shape = (String, usize, usize, bool, bool);

fn parser() -> Parser {
    let language = tree_sitter_ron::LANGUAGE.into();
    assert_eq!(tree_sitter::Language::abi_version(&language), 14);
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("retained ABI14 loads");
    parser
}

fn shape(tree: &Tree) -> Vec<Shape> {
    let mut result = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        result.push((
            node.kind().to_owned(),
            node.start_byte(),
            node.end_byte(),
            node.is_error(),
            node.is_missing(),
        ));
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return result;
            }
        }
    }
}

fn clean(source: &[u8], tree: &Tree) {
    let root = tree.root_node();
    assert_eq!(root.kind(), "source_file");
    assert_eq!(root.start_byte(), 0);
    assert_eq!(root.end_byte(), source.len());
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert!(shape(tree).iter().all(|node| !node.3 && !node.4));
}

fn edit_matches_fresh(source: &[u8], tree: &Tree) {
    let mut changed = source.to_vec();
    let start = source
        .iter()
        .position(|byte| matches!(byte, b'0'..=b'8'))
        .unwrap_or(source.len());
    let old_end = if start == source.len() {
        start
    } else {
        start + 1
    };
    if start == source.len() {
        changed.push(b' ');
    } else {
        changed[start] += 1;
    }
    let row = source[..start]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();
    let column = start
        - source[..start]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
    let mut edited = tree.clone();
    edited.edit(&InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: start + 1,
        start_position: Point::new(row, column),
        old_end_position: Point::new(row, column + old_end - start),
        new_end_position: Point::new(row, column + 1),
    });
    let incremental = parser()
        .parse(&changed, Some(&edited))
        .expect("incremental parse completes");
    let fresh = parser()
        .parse(&changed, None)
        .expect("fresh parse completes");
    clean(&changed, &incremental);
    clean(&changed, &fresh);
    assert_eq!(shape(&incremental), shape(&fresh));
}

#[test]
fn native_ron_loads_compounds_scanner_and_incremental_edits() {
    let source = br##"/* outer /* nested */ comment */
(name: "entry", ratio: 1.25e2,
 values: [Some(3), None, true, ()], pair: (1, 2), table: {"key": Path("a")})
"##;
    let tree = parser()
        .parse(source, None)
        .expect("native parse completes");
    clean(source, &tree);
    for kind in [
        "block_comment",
        "struct",
        "struct_entry",
        "string",
        "float",
        "array",
        "tuple",
        "map",
        "map_entry",
        "enum_variant",
    ] {
        assert!(
            shape(&tree).iter().any(|node| node.0 == kind),
            "{kind}: {}",
            tree.root_node().to_sexp()
        );
    }
    edit_matches_fresh(source, &tree);
}

#[test]
fn malformed_compounds_and_unsupported_forms_keep_native_errors_visible() {
    for source in [
        b"(1, 2".as_slice(),
        b"{\"key\": }".as_slice(),
        b"#![enable(implicit_some)]\n(value: 1)".as_slice(),
        br##"r#"raw value"#"##.as_slice(),
    ] {
        let tree = parser()
            .parse(source, None)
            .expect("recovery parse completes");
        assert!(
            tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert!(shape(&tree).iter().any(|node| node.3 || node.4));
    }
}

#[test]
#[ignore = "requires the explicitly supplied immutable original RON corpus"]
fn original_ron_corpus_parses_physical_bytes_and_matches_incremental_edits() {
    let root = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_RON_CORPUS").expect("external corpus root is required"),
    );
    let paths = [
        ".github/example-run/ambiguity_detection.ron",
        ".github/example-run/no_renderer.ron",
        ".github/example-run/testbed_2d.ron",
        ".github/example-run/testbed_3d.ron",
        ".github/example-run/testbed_ui.ron",
        ".github/example-run/world_serialization.ron",
        "assets/animation_graphs/Fox.animgraph.ron",
        "assets/data/simple_data.ron",
        "assets/serialized_worlds/load_scene_example.scn.ron",
        "examples/asset/processing/assets/a.cool.ron",
        "examples/asset/processing/assets/d.cool.ron",
        "examples/asset/processing/assets/foo/b.cool.ron",
        "examples/asset/processing/assets/foo/c.cool.ron",
    ];
    let mut bytes = 0;
    for path in paths {
        let source = std::fs::read(root.join(path)).expect("original physical source is readable");
        assert!(!source.is_empty() && source.len() <= 1024 * 1024);
        let tree = parser()
            .parse(&source, None)
            .expect("native corpus parse completes");
        clean(&source, &tree);
        edit_matches_fresh(&source, &tree);
        bytes += source.len();
        println!(
            "{path}: {} physical bytes, clean native tree and edit equivalence",
            source.len()
        );
    }
    assert_eq!(bytes, 3132);
}
