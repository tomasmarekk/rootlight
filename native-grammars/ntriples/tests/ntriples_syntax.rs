//! Verify native statement/term spans and format-specific syntax boundaries.
//! These tests do not qualify RDF graph interpretation or production routing.

use std::{fs, path::PathBuf};
use tree_sitter::{InputEdit, Parser, Point, Tree};

fn parser() -> Parser {
    let language: tree_sitter::Language = rootlight_tree_sitter_ntriples::LANGUAGE.into();
    assert_eq!(language.abi_version(), 14);
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    parser
}

fn parse(source: &[u8]) -> Tree {
    parser().parse(source, None).unwrap()
}

fn clean(source: &[u8], tree: &Tree) {
    assert_eq!(tree.root_node().byte_range(), 0..source.len());
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
}

fn shape(tree: &Tree) -> Vec<(String, usize, usize, bool, bool, bool)> {
    let mut result = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        result.push((
            node.kind().to_owned(),
            node.start_byte(),
            node.end_byte(),
            node.is_named(),
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
fn native_ntriples_preserves_statement_and_term_spans() {
    let source = "<https://example.test/s> <https://example.test/p> \"λ😀\"@en .\r\n".as_bytes();
    let tree = parse(source);
    let root = tree.root_node();
    assert_eq!(root.kind(), "document");
    assert_eq!(root.byte_range(), 0..source.len());
    assert!(!root.has_error(), "{}", root.to_sexp());
    let triple = root.named_child(0).unwrap();
    assert_eq!(triple.kind(), "triple");
    for (field, expected) in [
        ("subject", "<https://example.test/s>"),
        ("predicate", "<https://example.test/p>"),
        ("object", "\"λ😀\"@en"),
    ] {
        let term = triple.child_by_field_name(field).unwrap();
        assert_eq!(term.utf8_text(source).unwrap(), expected);
    }
}

#[test]
fn native_ntriples_rejects_turtle_relative_iris_and_quad_graph_terms() {
    for source in [
        b"@prefix ex: <https://example.test/> .\nex:s ex:p ex:o .\n".as_slice(),
        b"<s> <https://example.test/p> <https://example.test/o> .\n".as_slice(),
        b"<https://example.test/s> <p> <https://example.test/o> .\n".as_slice(),
        b"<https://example.test/s> <https://example.test/p> <o> .\n".as_slice(),
        b"<https://example.test/s> <https://example.test/p> \"v\"^^<type> .\n".as_slice(),
        b"<https://example.test/s> <https://example.test/p> <https://example.test/o> <https://example.test/g> .\n".as_slice(),
    ] {
        let tree = parse(source);
        assert!(tree.root_node().has_error(), "{}", tree.root_node().to_sexp());
    }
}

#[test]
fn native_ntriples_enforces_lines_and_keeps_comments_outside_terms() {
    let source = b"# header\r\n\r\n_:s<https://example.test/p>_:o.# tail\r\n\t_:s <https://example.test/p> \"v\" .# eof";
    let tree = parse(source);
    clean(source, &tree);
    let nodes = shape(&tree);
    assert_eq!(nodes.iter().filter(|node| node.0 == "triple").count(), 2);
    assert_eq!(nodes.iter().filter(|node| node.0 == "comment").count(), 3);
    for source in [
        b"_:s <https://example.test/p>\n _:o .\n".as_slice(),
        b"_:s <https://example.test/p> _:o . _:s <https://example.test/p> _:o .\n".as_slice(),
    ] {
        assert!(parse(source).root_node().has_error());
    }
}

#[test]
fn native_ntriples_distinguishes_permitted_nul_from_end_of_input() {
    let source = b"#\0 header\n_:s <https://example.test/p> \"\0\" .\n";
    let tree = parse(source);
    clean(source, &tree);
    assert!(
        parse(b"_:s <https://example.test/p> \"")
            .root_node()
            .has_error()
    );
    assert!(!parse(b"#\0 comment at eof").root_node().has_error());
}

#[test]
fn native_ntriples_preserves_literal_suffixes_and_unicode_escape_boundaries() {
    let source =
        br#"_:s <\u0068ttp\u003A//example.test/p> "v\t\uD7FF\uE000\U00010000\U0010FFFF"@en-US .
_:s <https://example.test/p> "not-a-boolean"^^<http://www.w3.org/2001/XMLSchema#boolean> .
"#;
    let tree = parse(source);
    clean(source, &tree);
    let root = tree.root_node();
    let first = root
        .named_child(0)
        .unwrap()
        .child_by_field_name("object")
        .unwrap();
    assert_eq!(first.kind(), "literal");
    assert_eq!(
        first
            .child_by_field_name("language")
            .unwrap()
            .utf8_text(source)
            .unwrap(),
        "@en-US"
    );
    let second = root
        .named_child(1)
        .unwrap()
        .child_by_field_name("object")
        .unwrap();
    assert_eq!(
        second
            .child_by_field_name("datatype")
            .unwrap()
            .utf8_text(source)
            .unwrap(),
        "<http://www.w3.org/2001/XMLSchema#boolean>"
    );
    for escape in [
        "\\uD800",
        "\\uDFFF",
        "\\U0000D800",
        "\\U00110000",
        "\\UFFFFFFFF",
        "\\x20",
    ] {
        let source = format!("_:s <https://example.test/p> \"{escape}\" .\n");
        assert!(parse(source.as_bytes()).root_node().has_error(), "{escape}");
    }
}

#[test]
fn native_ntriples_incremental_unicode_edit_matches_fresh_tree() {
    let source = b"_:s <https://example.test/p> \"old\" .\n_:s <https://example.test/p> _:o .\n";
    let mut original = parse(source);
    let start = source.windows(3).position(|bytes| bytes == b"old").unwrap();
    let replacement = "λ😀".as_bytes();
    let mut changed = source.to_vec();
    changed.splice(start..start + 3, replacement.iter().copied());
    original.edit(&InputEdit {
        start_byte: start,
        old_end_byte: start + 3,
        new_end_byte: start + replacement.len(),
        start_position: Point::new(0, start),
        old_end_position: Point::new(0, start + 3),
        new_end_position: Point::new(0, start + replacement.len()),
    });
    let incremental = parser().parse(&changed, Some(&original)).unwrap();
    let fresh = parse(&changed);
    clean(&changed, &incremental);
    clean(&changed, &fresh);
    assert_eq!(shape(&incremental), shape(&fresh));
}

fn explicit_corpus() -> PathBuf {
    PathBuf::from(
        std::env::var_os("ROOTLIGHT_NTRIPLES_CORPUS").expect("explicit pinned corpus parent"),
    )
}

#[test]
#[ignore = "requires explicitly supplied pinned original N-Triples corpus"]
fn original_w3c_ntriples_fixture_intent_is_preserved() {
    let root = explicit_corpus().join("183_N3_rdflib/test/data/suites/w3c/ntriples");
    let manifest = fs::read_to_string(root.join("manifest.ttl")).unwrap();
    let mut positive = 0;
    let mut negative = 0;
    let mut mismatches = Vec::new();
    for block in manifest.split("\n<#") {
        let expected = if block.contains("rdf:type rdft:TestNTriplesPositiveSyntax") {
            positive += 1;
            true
        } else if block.contains("rdf:type rdft:TestNTriplesNegativeSyntax") {
            negative += 1;
            false
        } else {
            continue;
        };
        let action = block.split_once("mf:action").unwrap().1;
        let name = action.split_once('<').unwrap().1.split_once('>').unwrap().0;
        assert!(!name.contains(['/', '\\']));
        let source = fs::read(root.join(name)).unwrap();
        let tree = parse(&source);
        if !tree.root_node().has_error() != expected {
            mismatches.push((name.to_owned(), tree.root_node().to_sexp()));
        }
        assert_eq!(tree.root_node().byte_range(), 0..source.len(), "{name}");
    }
    assert_eq!((positive, negative), (41, 27));
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    for name in ["literal_false.nt", "literal_true.nt"] {
        let source = fs::read(root.join(name)).unwrap();
        clean(&source, &parse(&source));
    }
}

#[test]
#[ignore = "requires explicitly supplied pinned original N-Triples corpus"]
fn original_schemaorg_ntriples_has_written_native_statement_fields() {
    let source =
        fs::read(explicit_corpus().join("182_Turtle_schemaorg/data/releases/3.9/ext-bib.nt"))
            .unwrap();
    assert_eq!(source.len(), 19_356);
    let tree = parse(&source);
    clean(&source, &tree);
    let root = tree.root_node();
    let mut cursor = root.walk();
    let statements: Vec<_> = root
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "triple")
        .collect();
    assert_eq!(statements.len(), 169);
    for statement in statements {
        for field in ["subject", "predicate", "object"] {
            let node = statement.child_by_field_name(field).unwrap();
            assert!(node.start_byte() < node.end_byte());
            assert!(source.get(node.byte_range()).is_some());
        }
    }
}
