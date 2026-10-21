use konomanoasa_tree_sitter_gsed::{HIGHLIGHTS_QUERY, LANGUAGE, LANGUAGE_ERE};
use tree_sitter::{InputEdit, Parser, Point, Query};

#[test]
fn languages_parse_gnu_scripts_and_highlight_queries_compile() {
  for (language, source) in [
    (LANGUAGE, r"s/\(a\|b\)\+/\U&/g"),
    (LANGUAGE_ERE, r"s/(a|b)+/\U&/g"),
  ] {
    let language = language.into();
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    Query::new(&language, HIGHLIGHTS_QUERY).unwrap();
  }
}

#[test]
fn delimiter_edit_matches_a_fresh_parse() {
  for language in [LANGUAGE, LANGUAGE_ERE] {
    let mut parser = Parser::new();
    parser.set_language(&language.into()).unwrap();
    let before = "s/a/b/";
    let after = "s/a/b";
    let mut old = parser.parse(before, None).unwrap();
    old.edit(&InputEdit {
      start_byte: 5,
      old_end_byte: 6,
      new_end_byte: 5,
      start_position: Point::new(0, 5),
      old_end_position: Point::new(0, 6),
      new_end_position: Point::new(0, 5),
    });
    let incremental = parser.parse(after, Some(&old)).unwrap();
    let fresh = parser.parse(after, None).unwrap();
    assert_eq!(
      incremental.root_node().to_sexp(),
      fresh.root_node().to_sexp()
    );
  }
}
