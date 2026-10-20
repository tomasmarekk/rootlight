//! Real Tree-sitter parser checks for the retained Racket tables and scanner.
//! Exact here-string ends must survive both fresh and incremental parsing.

fn parser() -> tree_sitter::Parser {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&super::LANGUAGE.into()).unwrap();
    parser
}

#[test]
fn default_reader_lists_and_delimiter_suffixes_remain_distinct() {
    for (source, end) in [
        ("#<<END\nEXTRA\nEND\n(define tail 1)", "#<<END\nEXTRA\nEND"),
        (
            "#<<END\nENDING\nEND\n(define tail 1)",
            "#<<END\nENDING\nEND",
        ),
        (
            "#<<ĀEND\nȀEXTRA\nĀEND\n(define tail 1)",
            "#<<ĀEND\nȀEXTRA\nĀEND",
        ),
        (
            "#<<🙂END\n🙂EXTRA\n🙂END\n(define tail 1)",
            "#<<🙂END\n🙂EXTRA\n🙂END",
        ),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let mut cursor = tree.root_node().walk();
        let nodes = tree
            .root_node()
            .named_children(&mut cursor)
            .collect::<Vec<_>>();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].kind(), "here_string");
        assert_eq!(nodes[0].end_byte(), end.len());
        assert_eq!(nodes[0].utf8_text(source.as_bytes()).unwrap(), end);
        assert_eq!(nodes[1].kind(), "list");
        assert_eq!(
            nodes[1].utf8_text(source.as_bytes()).unwrap(),
            "(define tail 1)"
        );
    }
}

#[test]
fn here_string_edits_match_fresh_parser_state() {
    let before = "#<<END\nBODY\nEND\n(define tail 1)";
    let after = "#<<END\nEXTRA\nEND\n(define tail 1)";
    let mut parser = parser();
    let mut tree = parser.parse(before, None).unwrap();
    let start = before.find("BODY").unwrap();
    tree.edit(&tree_sitter::InputEdit {
        start_byte: start,
        old_end_byte: start + 4,
        new_end_byte: start + 5,
        start_position: tree_sitter::Point::new(1, 0),
        old_end_position: tree_sitter::Point::new(1, 4),
        new_end_position: tree_sitter::Point::new(1, 5),
    });
    let edited = parser.parse(after, Some(&tree)).unwrap();
    let fresh = self::parser().parse(after, None).unwrap();
    assert!(!edited.root_node().has_error());
    assert_eq!(edited.root_node().to_sexp(), fresh.root_node().to_sexp());
    assert_eq!(edited.root_node().named_child(0).unwrap().end_byte(), 16);
}
