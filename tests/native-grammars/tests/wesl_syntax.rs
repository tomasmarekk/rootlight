//! Load and exercise the retained WESL parser and external scanner.
//! These syntax checks do not qualify production indexing or import resolution.

use tree_sitter::{InputEdit, Parser, Point, Tree};

type NodeShape = (String, usize, usize, bool, bool);

fn parser() -> Parser {
    let language = tree_sitter_wesl::LANGUAGE.into();
    assert_eq!(tree_sitter::Language::abi_version(&language), 15);
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("retained ABI15 grammar loads");
    parser
}

fn shape(tree: &Tree) -> Vec<NodeShape> {
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

#[test]
fn native_wesl_loads_imports_functions_scanner_and_incremental_edits() {
    let source = concat!(
        "/* outer /* nested */ comment */\n",
        "import package::math::{helper as calc};\n",
        "fn entry(value: f32) -> f32 {\n",
        "  let data = array<vec2<f32>, 2>(vec2<f32>(0.0), vec2<f32>(1.0));\n",
        "  return calc(value);\n",
        "}\n",
    );
    let mut parser = parser();
    let tree = parser.parse(source, None).expect("native parse completes");
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().kind(), "translation_unit");
    assert_eq!(tree.root_node().start_byte(), 0);
    assert_eq!(tree.root_node().end_byte(), source.len());
    for kind in [
        "block_comment",
        "import_statement",
        "function_decl",
        "template_list",
    ] {
        assert!(
            shape(&tree).iter().any(|node| node.0 == kind),
            "{kind}: {}",
            tree.root_node().to_sexp()
        );
    }
    let start = source.find("vec2").unwrap() + "vec".len();
    let mut changed = source.to_owned();
    changed.replace_range(start..start + 1, "4");
    let row = source[..start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    let column = start - source[..start].rfind('\n').map_or(0, |index| index + 1);
    let mut edited = tree.clone();
    edited.edit(&InputEdit {
        start_byte: start,
        old_end_byte: start + 1,
        new_end_byte: start + 1,
        start_position: Point::new(row, column),
        old_end_position: Point::new(row, column + 1),
        new_end_position: Point::new(row, column + 1),
    });
    let incremental = parser
        .parse(&changed, Some(&edited))
        .expect("incremental parse completes");
    let fresh = parser.parse(&changed, None).expect("fresh parse completes");
    assert!(!incremental.root_node().has_error());
    assert_eq!(shape(&incremental), shape(&fresh));
    let invalid = parser
        .parse("import ;\nfn broken( {\n", None)
        .expect("recovery parse completes");
    assert!(invalid.root_node().has_error());
}

#[test]
#[ignore = "requires the caller-supplied immutable external WESL original"]
fn original_wesl_witnesses_expose_exact_native_cst() {
    let path = std::env::var_os("ROOTLIGHT_WESL_EXTERNAL_SOURCE")
        .expect("explicit external source path is required");
    let external = std::fs::read(path).expect("external original is readable");
    for (name, source) in [
        ("external", external.as_slice()),
        ("manual_material", include_bytes!("../../../crates/rootlight-adapter-treesitter/tests/fixtures/wesl/manual_material.wesl").as_slice()),
        ("wireframe2d", include_bytes!("../../../crates/rootlight-adapter-treesitter/tests/fixtures/wesl/wireframe2d.wesl").as_slice()),
    ] {
        assert!(source.len() <= 1024 * 1024);
        let tree = parser().parse(source, None).expect("native parse completes");
        println!("{name} {}: {}", source.len(), tree.root_node().to_sexp());
        assert!(!tree.root_node().has_error());
        assert_eq!(tree.root_node().start_byte(), 0);
        assert_eq!(tree.root_node().end_byte(), source.len());
    }
}
