//! Original physical headers and native parser error/edit behavior remain explicit.
use tree_sitter::{InputEdit, Parser, Point};
const SOURCE: &str = include_str!("../../tests/fixtures/index.d");
fn parser() -> Parser {
    let mut p = Parser::new();
    p.set_language(&super::LANGUAGE.into()).unwrap();
    p
}
#[test]
fn original_native_identifiers_match_authored_headers() {
    let mut p = parser();
    let t = p.parse(SOURCE, None).unwrap();
    assert!(!t.root_node().has_error());
    assert_eq!(SOURCE.len(), 191);
    for (a, b, name) in [
        (7, 12, "index"),
        (52, 60, "showHome"),
        (79, 82, "req"),
        (103, 106, "res"),
    ] {
        let n = t.root_node().named_descendant_for_byte_range(a, b).unwrap();
        assert_eq!(n.kind(), "identifier");
        assert_eq!(n.byte_range(), a..b);
        assert_eq!(&SOURCE[a..b], name);
    }
}
#[test]
fn actual_native_edit_matches_fresh_tree() {
    let mut p = parser();
    let mut old = p.parse(SOURCE, None).unwrap();
    let updated = format!("{SOURCE}\n");
    let end = old.root_node().end_position();
    old.edit(&InputEdit {
        start_byte: SOURCE.len(),
        old_end_byte: SOURCE.len(),
        new_end_byte: updated.len(),
        start_position: end,
        old_end_position: end,
        new_end_position: Point::new(end.row + 1, 0),
    });
    let a = p.parse(&updated, Some(&old)).unwrap();
    let b = p.parse(&updated, None).unwrap();
    assert_eq!(a.root_node().to_sexp(), b.root_node().to_sexp());
    assert!(!b.root_node().has_error());
}
#[test]
fn malformed_native_header_is_not_clean() {
    let mut p = parser();
    for text in [
        "void missing( {",
        "struct Record { int ; }",
        r#"auto text = "missing;"#,
    ] {
        assert!(
            p.parse(text, None).unwrap().root_node().has_error(),
            "{text}"
        );
    }
}
