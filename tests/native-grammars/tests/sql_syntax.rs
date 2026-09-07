//! SQL source syntax and dollar delimiters through the actual native parser.
//! Incremental edits must preserve the same concrete tree and byte spans as fresh parses.

use tree_sitter::{InputEdit, Node, Parser, Point};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sequel::LANGUAGE.into())
        .unwrap();
    parser
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![root];
    let mut found = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            found.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    found.sort_by_key(Node::start_byte);
    found
}

#[test]
fn sql_declarations_have_complete_source_extents() {
    for (kind, source) in [
        (
            "create_table",
            "CREATE TABLE account (id INT PRIMARY KEY, name TEXT NOT NULL);",
        ),
        (
            "create_view",
            "CREATE VIEW active AS SELECT id FROM account WHERE id > 0;",
        ),
        ("create_index", "CREATE INDEX by_name ON account (name);"),
        (
            "create_function",
            "CREATE FUNCTION identity(value INT) RETURNS INT AS $body$ SELECT value; $body$ LANGUAGE SQL;",
        ),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let declarations = nodes(tree.root_node(), kind);
        assert_eq!(declarations.len(), 1, "{}", tree.root_node().to_sexp());
        assert_eq!(
            declarations[0].utf8_text(source.as_bytes()).unwrap(),
            source.trim_end_matches(';')
        );
    }
}

#[test]
fn sql_dollar_literals_preserve_unicode_overlaps_and_nuls() {
    for (tag, body) in [
        ("$$", "plain 'quoted' -- literal"),
        ("$tag$", "one$other"),
        ("$πЖ$", "Unicode: π Ж 💡"),
        ("$tag$", "one\0two"),
        ("$Name$", "case $name$ matters"),
        ("$.$", "permissive punctuation delimiter"),
        ("$$", "closing prefix $x"),
    ] {
        let literal = format!("{tag}{body}{tag}");
        let source = format!("SELECT {literal};");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source:?}: {}",
            tree.root_node().to_sexp()
        );
        assert!(
            nodes(tree.root_node(), "literal")
                .iter()
                .any(|node| node.utf8_text(source.as_bytes()).unwrap() == literal),
            "{}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn sql_nested_literal_does_not_replace_function_delimiter() {
    let source = "CREATE FUNCTION text_value() RETURNS TEXT AS $outer$ SELECT $inner$one$inner$; $outer$ LANGUAGE SQL;";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let tags = nodes(tree.root_node(), "dollar_quote");
    assert_eq!(tags.len(), 2);
    for tag in tags {
        assert_eq!(tag.utf8_text(source.as_bytes()).unwrap(), "$outer$");
    }
    assert!(
        nodes(tree.root_node(), "literal")
            .iter()
            .any(|node| node.utf8_text(source.as_bytes()).unwrap() == "$inner$one$inner$")
    );
}

#[test]
fn sql_incomplete_or_oversized_delimiters_remain_parse_errors() {
    for source in [
        "SELECT $a$unfinished;".to_owned(),
        "SELECT $bad tag$text$bad tag$;".to_owned(),
        format!("SELECT ${0}$text${0}$;", "a".repeat(1020)),
    ] {
        let tree = parser().parse(&source, None).unwrap();
        assert!(tree.root_node().has_error(), "{source}");
        assert_eq!(tree.root_node().end_byte(), source.len());
    }
}

#[test]
fn sql_incremental_edits_match_fresh_trees_and_source_spans() {
    for initial in [
        "SELECT $tag$one$other$tag$;",
        "CREATE FUNCTION text_value() RETURNS TEXT AS $outer$ SELECT $inner$one$inner$; $outer$ LANGUAGE SQL;",
    ] {
        let mut parser = parser();
        let mut source = initial.to_owned();
        let mut tree = parser.parse(&source, None).unwrap();
        assert!(!tree.root_node().has_error());
        for index in 0..24 {
            let start = source.find("one").unwrap();
            let replacement = if index % 2 == 0 { "one_plus" } else { "one" };
            let old_length = if source[start..].starts_with("one_plus") {
                8
            } else {
                3
            };
            let old_end = start + old_length;
            let new_end = start + replacement.len();
            tree.edit(&InputEdit {
                start_byte: start,
                old_end_byte: old_end,
                new_end_byte: new_end,
                start_position: Point::new(0, start),
                old_end_position: Point::new(0, old_end),
                new_end_position: Point::new(0, new_end),
            });
            source.replace_range(start..old_end, replacement);
            let incremental = parser.parse(&source, Some(&tree)).unwrap();
            let fresh = parser.parse(&source, None).unwrap();
            assert!(!fresh.root_node().has_error(), "{source}");
            assert_eq!(
                incremental.root_node().to_sexp(),
                fresh.root_node().to_sexp()
            );
            for kind in ["literal", "dollar_quote", "create_function"] {
                let spans = |root| {
                    nodes(root, kind)
                        .iter()
                        .map(Node::byte_range)
                        .collect::<Vec<_>>()
                };
                assert_eq!(spans(incremental.root_node()), spans(fresh.root_node()));
            }
            tree = incremental;
        }
    }
}
