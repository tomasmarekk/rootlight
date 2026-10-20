//! Exercise retained scanner tokens and incremental trees through the runtime.
//! EOF ends a final statement without weakening required block separators.

use super::LANGUAGE;

#[test]
fn stateless_scanner_forms_and_incremental_edits_match_fresh_parse() {
    let source = "# λ🙂\nfunction sample -a value\n echo pre\"$value\"suffix $argv[1] {one,two}suffix\n { echo brace; }\nend\n";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&LANGUAGE.into()).unwrap();
    let mut old = parser.parse(source, None).unwrap();
    assert!(
        !old.root_node().has_error(),
        "{}",
        old.root_node().to_sexp()
    );
    for kind in [
        "concatenation",
        "list_element_access",
        "brace_expansion",
        "begin_statement",
    ] {
        assert!(old.root_node().to_sexp().contains(kind), "{kind}");
    }
    let start = source.find("two").unwrap();
    let line_start = source[..start].rfind('\n').unwrap() + 1;
    let row = source[..start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    let point = tree_sitter::Point::new(row, start - line_start);
    old.edit(&tree_sitter::InputEdit {
        start_byte: start,
        old_end_byte: start + 3,
        new_end_byte: start + 5,
        start_position: point,
        old_end_position: tree_sitter::Point::new(row, point.column + 3),
        new_end_position: tree_sitter::Point::new(row, point.column + 5),
    });
    let changed = source.replace("two", "three");
    let incremental = parser.parse(&changed, Some(&old)).unwrap();
    let fresh = parser.parse(&changed, None).unwrap();
    assert!(!incremental.root_node().has_error());
    assert_eq!(
        incremental.root_node().to_sexp(),
        fresh.root_node().to_sexp()
    );
    assert_eq!(incremental.root_node().end_byte(), changed.len());
}

#[test]
fn final_statement_accepts_eof_but_retains_required_internal_separators() {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&LANGUAGE.into()).unwrap();
    for source in [
        "echo value",
        "echo value\n",
        "echo value;",
        "echo value | cat",
        "function written -a value\n echo $value\nend",
        "if true\n echo value\nend",
        "echo value # final comment",
    ] {
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        assert_eq!(tree.root_node().end_byte(), source.len());
    }
    for source in [
        "function written\n echo value end",
        "function written echo value end",
        "function written\n echo value\n",
        "echo value |",
    ] {
        assert!(
            parser.parse(source, None).unwrap().root_node().has_error(),
            "{source}"
        );
    }
}
