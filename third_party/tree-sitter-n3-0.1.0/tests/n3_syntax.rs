//! Native Notation3 formulas and variables keep physical source coordinates.
//! Recovery and edit equivalence do not establish rule execution or RDF inference.

use tree_sitter::{InputEdit, Parser, Point, Tree};

type Shape = (String, usize, usize, bool, bool, bool);

fn parser() -> Parser {
    let language: tree_sitter::Language = tree_sitter_n3::LANGUAGE.into();
    assert_eq!(language.abi_version(), 15);
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("retained ABI15 loads");
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
    assert_eq!(root.kind(), "n3_doc");
    assert_eq!(root.start_byte(), 0);
    assert_eq!(root.end_byte(), source.len());
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert!(shape(tree).iter().all(|node| !node.4 && !node.5));
}

#[test]
fn native_formulas_variables_spans_and_incremental_edits() {
    let source = concat!(
        "@prefix ex: <https://example.test/> .\r\n",
        "{ ?person ex:knows ?friend . { ?friend ex:age 21 . } ex:says true . } => ",
        "{ ?person ex:eligible \"λ😀\" . } .\r\n",
        "{ ?person ex:derived true . } <= { ?person ex:seed true . } .\r\n",
    )
    .as_bytes();
    let tree = parser().parse(source, None).expect("native parse");
    clean(source, &tree);
    let nodes = shape(&tree);
    assert_eq!(nodes.iter().filter(|node| node.0 == "formula").count(), 5);
    assert!(nodes.iter().any(|node| node.0 == "quick_var"));
    for operator in [b"=>".as_slice(), b"<="] {
        assert!(
            nodes
                .iter()
                .any(|node| { node.0 == "verb" && source.get(node.1..node.2) == Some(operator) })
        );
    }
    let literal = nodes.iter().find(|node| node.0 == "rdf_literal").unwrap();
    assert_eq!(&source[literal.1..literal.2], "\"λ😀\"".as_bytes());

    let start = source.windows(2).position(|bytes| bytes == b"21").unwrap();
    let mut changed = source.to_vec();
    changed[start + 1] = b'2';
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
        old_end_byte: start + 2,
        new_end_byte: start + 2,
        start_position: Point::new(row, column),
        old_end_position: Point::new(row, column + 2),
        new_end_position: Point::new(row, column + 2),
    });
    let incremental = parser().parse(&changed, Some(&edited)).unwrap();
    let fresh = parser().parse(&changed, None).unwrap();
    clean(&changed, &incremental);
    clean(&changed, &fresh);
    assert_eq!(shape(&incremental), shape(&fresh));
}

#[test]
fn malformed_notation3_retains_native_recovery() {
    for source in [
        b"@prefix ex: <https://example.test/> .\n{ ?s ex:p ex:o . => { ?s ex:q ex:o . } .\n"
            .as_slice(),
        b"@prefix ex: <https://example.test/> .\n? ex:p ex:o .\n",
        b"@prefix ex: <https://example.test/> .\nex:s ex:p .\n",
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
fn legacy_quantifiers_keep_symbol_lists_and_formula_scope() {
    let source = concat!(
        "@prefix ex: <https://example.test/> .\n",
        "@forAll <urn:x>, ex:y .\n@forSome .\n",
        "{ @forAll . @forSome ex:inner, <urn:another> . ex:inner ex:p true } => ",
        "{ ex:inner ex:q true } .\n",
    )
    .as_bytes();
    let tree = parser().parse(source, None).unwrap();
    clean(source, &tree);
    let nodes = shape(&tree);
    assert_eq!(nodes.iter().filter(|node| node.0 == "universal").count(), 2);
    assert_eq!(
        nodes.iter().filter(|node| node.0 == "existential").count(),
        2
    );
    for invalid in [
        b"@forAll <urn:x>, .\n".as_slice(),
        b"@forSome , <urn:x> .\n",
        b"@forAll \"literal\" .\n",
        b"@forall <urn:x> .\n",
    ] {
        let refused = parser().parse(invalid, None).unwrap();
        assert!(
            refused.root_node().has_error(),
            "{}",
            refused.root_node().to_sexp()
        );
    }
}

#[test]
#[ignore = "requires the original N3 corpus checkout"]
fn original_legacy_quantifiers_keep_exact_physical_directives() {
    let corpus = std::env::var_os("ROOTLIGHT_DIAGNOSTIC_CORPUS_ROOT")
        .expect("original corpus root is provided");
    let root = std::path::Path::new(&corpus).join("45_Prolog_eye");
    for (path, expected) in [
        (
            "reasoning/euq/euq.n3",
            [(97, 111, "@forAll <#X> .")].as_slice(),
        ),
        (
            "reasoning/sequents/proof/beetle.n3",
            [
                (1154, 1171, "@forSome var:x_0."),
                (1311, 1336, "@forAll var:x_0, var:x_1."),
            ]
            .as_slice(),
        ),
        (
            "reasoning/rgb/blueproof003.n3",
            [
                (1665, 1691, "@forSome var:x_0, var:x_1."),
                (2012, 2038, "@forSome var:x_0, var:x_1."),
            ]
            .as_slice(),
        ),
    ] {
        let source = std::fs::read(root.join(path)).expect("pinned original is readable");
        let tree = parser()
            .parse(&source, None)
            .expect("native original parse");
        clean(&source, &tree);
        let nodes = shape(&tree);
        for (start, end, written) in expected {
            assert_eq!(source.get(*start..*end), Some(written.as_bytes()), "{path}");
            let directive = written.strip_suffix('.').unwrap().trim_end();
            assert!(
                nodes.iter().any(|node| {
                    matches!(node.0.as_str(), "universal" | "existential")
                        && node.1 == *start
                        && node.2 == start + directive.len()
                }),
                "{path}: exact legacy directive node"
            );
        }
    }
}
