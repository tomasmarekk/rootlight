//! Native SPARQL syntax retains physical coordinates across queries and updates.
//! These controls do not execute requests or certify semantic validity.

use tree_sitter::{InputEdit, Parser, Point, Tree};

type Shape = (String, usize, usize, bool, bool, bool);

fn parser() -> Parser {
    let language: tree_sitter::Language = tree_sitter_sparql::LANGUAGE.into();
    assert_eq!(language.abi_version(), 15);
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("retained upstream ABI15 loads");
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
    let root = tree.root_node();
    assert_eq!(root.kind(), "unit");
    assert_eq!(root.start_byte(), 0);
    assert_eq!(root.end_byte(), source.len());
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert!(shape(tree).iter().all(|node| !node.4 && !node.5));
}

fn exact_node(tree: &Tree, source: &[u8], kind: &str, start: usize, end: usize, written: &[u8]) {
    assert_eq!(source.get(start..end), Some(written));
    assert!(
        shape(tree)
            .iter()
            .any(|node| node.0 == kind && node.1 == start && node.2 == end),
        "missing {kind}[{start},{end}): {}",
        tree.root_node().to_sexp()
    );
}

fn edit_equivalent(source: &[u8], start: usize, replacement: u8) {
    let tree = parser().parse(source, None).unwrap();
    clean(source, &tree);
    let mut changed = source.to_vec();
    changed[start] = replacement;
    let row = source[..start]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();
    let column = start
        - source[..start]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1);
    let mut edited = tree.clone();
    edited.edit(&InputEdit {
        start_byte: start,
        old_end_byte: start + 1,
        new_end_byte: start + 1,
        start_position: Point::new(row, column),
        old_end_position: Point::new(row, column + 1),
        new_end_position: Point::new(row, column + 1),
    });
    let incremental = parser().parse(&changed, Some(&edited)).unwrap();
    let fresh = parser().parse(&changed, None).unwrap();
    clean(&changed, &incremental);
    clean(&changed, &fresh);
    assert_eq!(shape(&incremental), shape(&fresh));
}

#[test]
fn queries_and_updates_keep_native_physical_coordinates() {
    let source = concat!(
        "PREFIX ex: <https://example.test/>\r\n",
        "sElEcT ?person (?age + 1 AS ?next) WHERE {\r\n",
        "  ?person ex:knows+ ?friend .\r\n",
        "  OPTIONAL { ?friend ex:label \"λ😀\" . }\r\n",
        "  BIND(?age AS ?copy) FILTER(?age > 0)\r\n",
        "} LIMIT 10\r\n",
        "INSERT DATA { <urn:s> <urn:p> \"λ😀\" . } ;\r\n",
        "DELETE DATA { <urn:s> <urn:p> \"λ😀\" . }\r\n",
    )
    .as_bytes();
    let tree = parser().parse(source, None).unwrap();
    clean(source, &tree);
    let nodes = shape(&tree);
    for kind in [
        "prefix_declaration",
        "select_query",
        "bind",
        "optional_graph_pattern",
        "insert_data",
        "delete_data",
    ] {
        assert!(nodes.iter().any(|node| node.0 == kind), "{kind}");
    }
    let path = b"ex:knows+";
    let start = source
        .windows(path.len())
        .position(|bytes| bytes == path)
        .unwrap();
    exact_node(
        &tree,
        source,
        "path_element",
        start,
        start + path.len(),
        path,
    );
    let literal = "\"λ😀\"".as_bytes();
    let start = source
        .windows(literal.len())
        .position(|bytes| bytes == literal)
        .unwrap();
    exact_node(
        &tree,
        source,
        "string",
        start,
        start + literal.len(),
        literal,
    );
}

#[test]
fn incremental_edit_matches_fresh_after_multibyte_crlf_source() {
    let source =
        "# λ😀\r\nPREFIX ex: <urn:example:>\r\nSELECT ?x WHERE { ?x ex:p 21 . }\r\n".as_bytes();
    let start = source.windows(2).position(|bytes| bytes == b"21").unwrap() + 1;
    edit_equivalent(source, start, b'2');
}

#[test]
fn empty_construct_and_update_forms_keep_nonempty_native_contexts() {
    for source in [
        "CONSTRUCT {} WHERE {}\n",
        "CONSTRUCT {} {}\n",
        "BASE <http://example/>\n# Otherwise empty\n",
        "PREFIX : <http://example/>\n# Otherwise empty\n",
        "# prefix comment\r\nBASE <urn:example:>\r\nPREFIX ex: <urn:example:>\r\n",
        "PREFIX ex: <urn:example:> SELECT ?s WHERE { ?s ex:p ex:o }\n",
        "INSERT DATA {};\n",
        "INSERT DATA {}; BASE <urn:example:>\n# empty tail\n",
        "PREFIX ex: <urn:example:> INSERT DATA {}; PREFIX other: <urn:other:> DELETE DATA {}\n",
    ] {
        let tree = parser().parse(source, None).unwrap();
        clean(source.as_bytes(), &tree);
        if source.starts_with("CONSTRUCT") {
            exact_node(
                &tree,
                source.as_bytes(),
                "construct_template",
                10,
                12,
                b"{}",
            );
        }
        if source.contains("SELECT") {
            let nodes = shape(&tree);
            assert_eq!(
                nodes.iter().filter(|node| node.0 == "select_query").count(),
                1
            );
            assert_eq!(
                nodes
                    .iter()
                    .filter(|node| node.0 == "prefix_declaration")
                    .count(),
                1
            );
            assert!(
                nodes
                    .iter()
                    .all(|node| node.0 != "insert_data" && node.0 != "delete_data")
            );
        }
        if source.contains("BASE") && source.contains("PREFIX") {
            assert_eq!(
                shape(&tree)
                    .iter()
                    .filter(|node| node.0 == "prologue")
                    .count(),
                1
            );
        }
    }
}

#[test]
fn malformed_queries_and_updates_retain_native_recovery() {
    for source in [
        b"SELECT ?s WHERE { ?s <urn:p> . }\n".as_slice(),
        b"SELECT ?s WHERE { ?s <urn:p> <urn:o> .\n",
        b"INSERT DATA { <urn:s> <urn:p> }\n",
        b"DELETE DATA { <urn:s> <urn:p> <urn:o>\n",
        b"CONSTRUCT {}\n",
        b"CONSTRUCT {} WHERE {\n",
        b"CONSTRUCT { WHERE {}\n",
        b"BASE\n",
        b"PREFIX :\n",
        b"PREFIX : <http://example/> trailing_junk\n",
        b"INSERT DATA {};;\n",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
#[ignore = "requires an explicitly supplied original SPARQL corpus root"]
fn original_approved_empty_construct_and_updates_are_native_clean() {
    let corpus = std::env::var_os("ROOTLIGHT_DIAGNOSTIC_CORPUS_ROOT")
        .expect("original corpus root is provided");
    let root = std::path::Path::new(&corpus);
    let witnesses = [
        (
            "sparql10/syntax-sparql2/syntax-form-construct06.rq",
            "dawg-data-r2/syntax-sparql2/syntax-form-construct06.rq",
            22,
        ),
        (
            "sparql11/syntax-update-1/syntax-update-39.ru",
            "sparql11/syntax-update-1/syntax-update-39.ru",
            41,
        ),
        (
            "sparql11/syntax-update-1/syntax-update-40.ru",
            "sparql11/syntax-update-1/syntax-update-40.ru",
            45,
        ),
    ];
    for (jena, rdflib, expected_bytes) in witnesses {
        let first =
            std::fs::read(root.join("183_N3_rdflib/test/data/suites/w3c").join(rdflib)).unwrap();
        let second = std::fs::read(
            root.join("184_SPARQL_jena/jena-arq/testing/rdf-tests-cg/sparql")
                .join(jena),
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), expected_bytes);
        for source in [&first, &second] {
            let tree = parser().parse(source, None).unwrap();
            clean(source, &tree);
        }
    }
}

#[test]
#[ignore = "requires an explicitly supplied original RDFLib corpus root"]
fn original_w3c_queries_and_update_keep_exact_physical_terms() {
    let corpus = std::env::var_os("ROOTLIGHT_DIAGNOSTIC_CORPUS_ROOT")
        .expect("original corpus root is provided");
    let root = std::path::Path::new(&corpus).join("183_N3_rdflib/test/data/suites/w3c");
    let property = std::fs::read(root.join("sparql11/property-path/path-2-2.rq")).unwrap();
    assert_eq!(property.len(), 68);
    let tree = parser().parse(&property, None).unwrap();
    clean(&property, &tree);
    exact_node(&tree, &property, "path_element", 56, 59, b":p+");
    exact_node(&tree, &property, "var", 60, 62, b"?z");
    edit_equivalent(&property, 61, b'w');

    let update = std::fs::read(root.join("sparql11/basic-update/insert-data-spo1.ru")).unwrap();
    assert_eq!(update.len(), 60);
    let tree = parser().parse(&update, None).unwrap();
    clean(&update, &tree);
    assert_eq!(update.get(35..46), Some(b"INSERT DATA".as_slice()));
    assert!(
        shape(&tree)
            .iter()
            .any(|node| node.0 == "insert_data" && node.1 == 35)
    );

    // This retained original provides genuine Unicode identifiers with CRLF.
    let unicode = std::fs::read(root.join("dawg-data-r2/i18n/kanji-02.rq")).unwrap();
    assert_eq!(unicode.len(), 280);
    let tree = parser().parse(&unicode, None).unwrap();
    clean(&unicode, &tree);
    for (kind, start, end, written) in [
        ("pn_prefix", 127, 130, "食"),
        ("prefixed_name", 248, 261, "食:食べる"),
        ("prefixed_name", 262, 272, "食:海老"),
        ("var", 206, 211, "?name"),
    ] {
        exact_node(&tree, &unicode, kind, start, end, written.as_bytes());
    }
}
