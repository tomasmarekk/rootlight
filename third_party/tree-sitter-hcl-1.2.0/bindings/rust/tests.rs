//! Selected HCL native tables preserve physical keys and incremental syntax.
use tree_sitter::{InputEdit, Parser, Point};
const SOURCE: &str = include_str!("../../tests/fixtures/versions.tf");
fn parser() -> Parser {
    let mut parser = Parser::new();
    parser.set_language(&super::LANGUAGE.into()).unwrap();
    parser
}
#[test]
fn original_native_keys_match_physical_property_witnesses() {
    assert_eq!(SOURCE.len(), 946);
    let mut parser = parser();
    let tree = parser.parse(SOURCE, None).unwrap();
    assert!(!tree.root_node().has_error());
    for (start, end, name) in [
        (75, 81, "hcloud"),
        (93, 99, "source"),
        (132, 139, "version"),
    ] {
        let key = tree
            .root_node()
            .named_descendant_for_byte_range(start, end)
            .unwrap();
        assert_eq!(key.kind(), "identifier");
        assert_eq!(key.byte_range(), start..end);
        assert_eq!(&SOURCE[start..end], name);
    }
    assert_ne!(
        tree.root_node()
            .named_descendant_for_byte_range(117, 123)
            .unwrap()
            .kind(),
        "identifier"
    );
}
#[test]
fn ordinary_templates_and_actual_edits_keep_incremental_trees() {
    let source = "value = \"hello ${var.name}\"\n";
    let mut parser = parser();
    let mut original = parser.parse(source, None).unwrap();
    assert!(!original.root_node().has_error());
    let updated = format!("{source}after = 1\n");
    let end = original.root_node().end_position();
    original.edit(&InputEdit {
        start_byte: source.len(),
        old_end_byte: source.len(),
        new_end_byte: updated.len(),
        start_position: end,
        old_end_position: end,
        new_end_position: Point::new(end.row + 1, 0),
    });
    let incremental = parser.parse(&updated, Some(&original)).unwrap();
    let fresh = parser.parse(&updated, None).unwrap();
    assert_eq!(
        incremental.root_node().to_sexp(),
        fresh.root_node().to_sexp()
    );
    assert_eq!(incremental.root_node().end_byte(), updated.len());
    assert!(!fresh.root_node().has_error());
}
#[test]
fn malformed_attribute_object_and_template_are_not_clean() {
    let mut parser = parser();
    for source in ["value =\n", "value = {child=1\n", "value = \"${var.name\n"] {
        assert!(
            parser.parse(source, None).unwrap().root_node().has_error(),
            "{source}"
        );
    }
}
