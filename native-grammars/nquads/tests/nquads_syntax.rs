//! Check physical N-Quads statement/term boundaries and strict lexical refusals.
//! Dataset interpretation, Dgraph dialect support and product routing are separate.

use std::{fs, path::PathBuf};

use tree_sitter::{InputEdit, Parser, Point, Tree};

type Shape = (String, usize, usize, bool, bool, bool);

fn parser() -> Parser {
    let language: tree_sitter::Language = rootlight_tree_sitter_nquads::LANGUAGE.into();
    assert_eq!(language.abi_version(), 14);
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    parser
}

fn parse(source: &[u8]) -> Tree {
    parser().parse(source, None).unwrap()
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

fn clean(source: &[u8], tree: &Tree) {
    assert_eq!(tree.root_node().kind(), "document");
    assert_eq!(tree.root_node().byte_range(), 0..source.len());
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert!(shape(tree).iter().all(|node| !node.4 && !node.5));
}

#[test]
fn default_and_named_graph_statements_preserve_three_or_four_physical_terms() {
    for (statement, subject, predicate, object, graph) in [
        (
            "<urn:s><urn:p><urn:o>.",
            "<urn:s>",
            "<urn:p>",
            "<urn:o>",
            None,
        ),
        (
            "_:s <urn:p> _:o <urn:g#anchor> .",
            "_:s",
            "<urn:p>",
            "_:o",
            Some("<urn:g#anchor>"),
        ),
        (
            "<urn:s> <urn:p> \"λ😀\"@en-US _:g .",
            "<urn:s>",
            "<urn:p>",
            "\"λ😀\"@en-US",
            Some("_:g"),
        ),
        (
            "_:s <urn:p> \"v\"^^<urn:type#x> .",
            "_:s",
            "<urn:p>",
            "\"v\"^^<urn:type#x>",
            None,
        ),
        (
            "_:s<urn:p>_:shared _:shared.",
            "_:s",
            "<urn:p>",
            "_:shared",
            Some("_:shared"),
        ),
    ] {
        let source = format!("# header\r\n{statement}# tail\n");
        let tree = parse(source.as_bytes());
        clean(source.as_bytes(), &tree);
        let mut cursor = tree.root_node().walk();
        let quad = tree
            .root_node()
            .named_children(&mut cursor)
            .find(|node| node.kind() == "quad")
            .unwrap();
        assert_eq!(quad.utf8_text(source.as_bytes()).unwrap(), statement);
        for (field, expected) in [
            ("subject", subject),
            ("predicate", predicate),
            ("object", object),
        ] {
            let term = quad.child_by_field_name(field).unwrap();
            assert_eq!(term.utf8_text(source.as_bytes()).unwrap(), expected);
        }
        let actual = quad.child_by_field_name("graph");
        assert_eq!(
            actual.map(|node| node.utf8_text(source.as_bytes()).unwrap()),
            graph
        );
        if let Some(graph) = actual {
            assert!(graph.start_byte() >= quad.child_by_field_name("object").unwrap().end_byte());
        }
        assert_eq!(
            shape(&tree).iter().filter(|node| node.0 == "quad").count(),
            1
        );
    }
}

#[test]
fn complete_iri_tokens_keep_hashes_and_literal_suffixes_outside_comments() {
    let source =
        r##"<urn:s#subject><urn:p#predicate>"#value"^^<urn:type#datatype><urn:g#graph>.# tail
_:λ <\u0068ttp\u003A//example.test/p> "v\t\uD7FF\uE000\U00010000\U0010FFFF"@en-US _:図.
"##
        .as_bytes();
    let tree = parse(source);
    clean(source, &tree);
    let nodes = shape(&tree);
    assert_eq!(nodes.iter().filter(|node| node.0 == "quad").count(), 2);
    let comments: Vec<_> = nodes.iter().filter(|node| node.0 == "comment").collect();
    assert_eq!(comments.len(), 1);
    assert_eq!(&source[comments[0].1..comments[0].2], b"# tail");
    assert_eq!(
        nodes.iter().filter(|node| node.0 == "language_tag").count(),
        1
    );
}

#[test]
fn raw_nul_is_data_in_strings_comments_and_eof_without_truncating_following_quads() {
    for ending in ["", "\n", "\r", "\r\n"] {
        let source = format!("#\0 header\r\n_:s<urn:p>\"#λ\0😀#\"_:g.#\0 tail{ending}");
        let tree = parse(source.as_bytes());
        clean(source.as_bytes(), &tree);
        assert_eq!(
            shape(&tree).iter().filter(|node| node.0 == "quad").count(),
            1
        );
        let string = shape(&tree)
            .into_iter()
            .find(|node| node.0 == "string")
            .unwrap();
        assert_eq!(
            &source.as_bytes()[string.1..string.2],
            "\"#λ\0😀#\"".as_bytes()
        );
    }
    let source = b"_:s<urn:p>\"\0\".\n#\0 comment\r\n_:tail<urn:p>_:o<urn:g>.";
    clean(source, &parse(source));
    assert_eq!(
        shape(&parse(source))
            .iter()
            .filter(|node| node.0 == "quad")
            .count(),
        2
    );
    for source in [b"".as_slice(), b"#\0 comment at eof", b"\r\n# comment\n"] {
        clean(source, &parse(source));
    }
}

#[test]
fn invalid_graphs_extra_terms_relative_iris_and_malformed_literals_are_refused() {
    for source in [
        "<urn:s><urn:p><urn:o>\"graph\".",
        "<urn:s><urn:p><urn:o><urn:g><urn:extra>.",
        "<urn:s>_:p<urn:o>.",
        "\"subject\"<urn:p><urn:o>.",
        "<s><urn:p><urn:o>.",
        "<urn:s><p><urn:o>.",
        "<urn:s><urn:p><o>.",
        "<urn:s><urn:p><urn:o><g>.",
        "<urn:s><urn:p>\"v\"^^<type>.",
        "<urn:s><urn:p><urn:o><#graph>.",
        "<urn:s><urn:p><urn:o>\n<urn:g>.",
        "<urn:s>\n<urn:p><urn:o>.",
        "<urn:s><urn:p><urn:o>. <urn:s><urn:p><urn:o>.",
        "<urn:s><urn:p><urn:o>",
        "<urn:s><urn:p>. <urn:g>",
        "<urn:s><urn:p>\"\0\\q\".",
        "<urn:s><urn:p>\"\\uD800\".",
        "<urn:s><urn:p>\"\\U00110000\".",
        "<urn:s><urn:p>\"\0",
        "<urn:s><urn:p>\"raw\nline\".",
        "<urn:s><urn:p>\"raw\rline\".",
        "<urn:s><urn:p>'''long''' .",
        "<urn:s><urn:p>'single' .",
        "<urn:s><urn:p><urn:o\0>.",
        "<urn:s><urn:p><urn:o><urn:g\0>.",
        "<urn:s>\0<urn:p><urn:o>.",
        "_:s\0<urn:p>_:o.",
        "<urn:s><urn:p>\"v\" @en.",
        "GRAPH <urn:g> { <urn:s><urn:p><urn:o>. }",
        "@prefix ex:<urn:ex:>. ex:s ex:p ex:o.",
    ] {
        let tree = parse(source.as_bytes());
        assert_eq!(tree.root_node().byte_range(), 0..source.len(), "{source:?}");
        assert!(
            tree.root_node().has_error(),
            "{source:?}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn incremental_unicode_and_graph_edits_match_fresh_native_trees() {
    for (source, marker, replacement) in [
        ("_:s<urn:p>\"old\"_:g.\n", "old", "λ\0😀"),
        ("_:s<urn:p>\"v\"<urn:old>.\n", "urn:old", "urn:new#graph"),
    ] {
        let mut tree = parse(source.as_bytes());
        clean(source.as_bytes(), &tree);
        let start = source.find(marker).unwrap();
        let mut changed = source.as_bytes().to_vec();
        changed.splice(
            start..start + marker.len(),
            replacement.as_bytes().iter().copied(),
        );
        tree.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + marker.len(),
            new_end_byte: start + replacement.len(),
            start_position: Point::new(0, start),
            old_end_position: Point::new(0, start + marker.len()),
            new_end_position: Point::new(0, start + replacement.len()),
        });
        let incremental = parser().parse(&changed, Some(&tree)).unwrap();
        let fresh = parse(&changed);
        clean(&changed, &incremental);
        clean(&changed, &fresh);
        assert_eq!(shape(&incremental), shape(&fresh));
    }
}

fn corpus() -> PathBuf {
    PathBuf::from(
        std::env::var_os("ROOTLIGHT_NQUADS_CORPUS").expect("explicit pinned corpus parent"),
    )
}

#[test]
#[ignore = "requires explicitly supplied pinned original N-Quads corpus"]
fn original_w3c_nquads_syntax_intent_and_complete_extents_are_preserved() {
    let root = corpus().join("183_N3_rdflib/test/data/suites/w3c/nquads");
    let manifest = fs::read_to_string(root.join("manifest.ttl")).unwrap();
    let mut positive = 0;
    let mut negative = 0;
    for block in manifest.split("\n<#") {
        let valid = if block.contains("a rdft:TestNQuadsPositiveSyntax") {
            positive += 1;
            true
        } else if block.contains("a rdft:TestNQuadsNegativeSyntax") {
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
        assert_eq!(tree.root_node().byte_range(), 0..source.len(), "{name}");
        assert_eq!(
            !tree.root_node().has_error(),
            valid,
            "{name}: {}",
            tree.root_node().to_sexp()
        );
    }
    assert_eq!((positive, negative), (53, 32));
}

#[test]
#[ignore = "requires explicitly supplied original N-Quads and Dgraph source intake"]
fn original_raw_nul_and_named_graphs_are_standard_while_dgraph_ids_remain_dialect_negative() {
    let parent = corpus();
    for (name, bytes, named) in [
        ("literal_ascii_boundaries.nq", 57, false),
        ("nq-syntax-bnode-01.nq", 62, true),
    ] {
        let source = fs::read(
            parent
                .join("183_N3_rdflib/test/data/suites/w3c/nquads")
                .join(name),
        )
        .unwrap();
        assert_eq!(source.len(), bytes);
        let tree = parse(&source);
        clean(&source, &tree);
        assert_eq!(
            tree.root_node()
                .named_child(0)
                .unwrap()
                .child_by_field_name("graph")
                .is_some(),
            named
        );
    }
    let source = fs::read(parent.join("187_NQuads_dgraph/dgraph/cmd/alpha/testdata.nq")).unwrap();
    assert_eq!(source.len(), 316);
    assert_eq!(
        source
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .count(),
        11
    );
    assert!(source.starts_with(b"<alice> <follows> <bob> .\n"));
    let tree = parse(&source);
    assert_eq!(tree.root_node().byte_range(), 0..source.len());
    assert!(
        tree.root_node().has_error(),
        "relative identifiers require a separate native dialect"
    );
}
