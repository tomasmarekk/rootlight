//! Check native physical spans, syntax refusal and edit equivalence.
//! This is parser behavior, independent of production routing and RDF inference.

use tree_sitter::{InputEdit, Parser, Point, Tree};

type Shape = (String, usize, usize, bool, bool, bool);

fn parser() -> Parser {
    let language: tree_sitter::Language = tree_sitter_turtle::LANGUAGE.into();
    assert_eq!(language.abi_version(), 14);
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("retained ABI14 loads");
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
    assert_eq!(root.kind(), "document");
    assert_eq!(root.start_byte(), 0);
    assert_eq!(root.end_byte(), source.len());
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert!(shape(tree).iter().all(|node| !node.4 && !node.5));
}

#[test]
fn retained_turtle_parser_preserves_spans_errors_and_edits() {
    let source = "@prefix ex: <https://example.test/> .\r\nex:s ex:p \"λ😀\" .\r\n".as_bytes();
    let tree = parser().parse(source, None).expect("native parse");
    clean(source, &tree);
    let nodes = shape(&tree);
    assert_eq!(nodes.iter().filter(|node| node.0 == "prefix_id").count(), 1);
    assert_eq!(nodes.iter().filter(|node| node.0 == "triple").count(), 1);
    let literal = nodes.iter().find(|node| node.0 == "rdf_literal").unwrap();
    assert_eq!(&source[literal.1..literal.2], "\"λ😀\"".as_bytes());

    let start = source
        .windows(4)
        .position(|bytes| bytes == b"ex:s")
        .unwrap()
        + 3;
    let mut changed = source.to_vec();
    changed[start] = b't';
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
    let incremental = parser()
        .parse(&changed, Some(&edited))
        .expect("incremental parse");
    let fresh = parser().parse(&changed, None).expect("fresh changed parse");
    clean(&changed, &incremental);
    clean(&changed, &fresh);
    assert_eq!(shape(&incremental), shape(&fresh));

    for invalid in [
        b"@prefix ex: <https://example.test/> .\nex:s ex:p .\n".as_slice(),
        b"@prefix ex: <https://example.test/> .\n{ ex:s ex:p ex:o . } => { ex:s ex:q ex:o . } .\n"
            .as_slice(),
    ] {
        let refused = parser().parse(invalid, None).expect("native recovery tree");
        assert!(
            refused.root_node().has_error(),
            "{}",
            refused.root_node().to_sexp()
        );
    }
}

#[test]
fn trig_graph_keyword_is_case_insensitive_with_named_native_labels() {
    let prefix = "@prefix ex: <https://example.test/> .\n";
    for keyword in ["GRAPH", "graph", "GrApH"] {
        let source = format!("{prefix}{keyword} ex:g {{ ex:s ex:p ex:o }}\n");
        let tree = parser().parse(&source, None).unwrap();
        clean(source.as_bytes(), &tree);
        let mut cursor = tree.root_node().walk();
        let graph = tree
            .root_node()
            .named_children(&mut cursor)
            .find(|node| node.kind() == "graph")
            .unwrap();
        let mut labels = graph.walk();
        let label = graph
            .children_by_field_name("label", &mut labels)
            .find(|node| node.is_named())
            .unwrap();
        assert_eq!(label.kind(), "prefixed_name");
        assert_eq!(label.utf8_text(source.as_bytes()).unwrap(), "ex:g");
    }
    let source = format!("{prefix}GRAPH ex:g {{ ex:s ex:p ex:o }}\n");
    let mut edited = parser().parse(&source, None).unwrap();
    let mut changed = source.into_bytes();
    let start = prefix.len();
    changed[start..start + 5].copy_from_slice(b"graph");
    edited.edit(&InputEdit {
        start_byte: start,
        old_end_byte: start + 5,
        new_end_byte: start + 5,
        start_position: Point::new(1, 0),
        old_end_position: Point::new(1, 5),
        new_end_position: Point::new(1, 5),
    });
    let incremental = parser().parse(&changed, Some(&edited)).unwrap();
    let fresh = parser().parse(&changed, None).unwrap();
    clean(&changed, &incremental);
    clean(&changed, &fresh);
    assert_eq!(shape(&incremental), shape(&fresh));
}

#[test]
fn trig_graph_keyword_requires_a_label_and_does_not_accept_longer_keywords() {
    for source in [
        b"GRAPH { }".as_slice(),
        b"graph { }".as_slice(),
        b"GRAPHIC <https://example.test/g> { }".as_slice(),
    ] {
        assert!(
            parser()
                .parse(source, None)
                .unwrap()
                .root_node()
                .has_error()
        );
    }
}

#[test]
fn fragment_iris_keep_leading_hashes_separate_from_nul_comments() {
    let source = b"@base <#base> .\n@prefix ex: <##namespace> .\n<#subject> <##predicate> <###object> .\n<#tail> <#p> \"a\0b\"^^<#type> . # comment\0\n<#> <> <##> .\n<x#anchor> <#p> <#> .";
    let tree = parser().parse(source, None).unwrap();
    clean(source, &tree);
    let nodes = shape(&tree);
    assert_eq!(nodes.iter().filter(|node| node.0 == "triple").count(), 4);
    let comments: Vec<_> = nodes.iter().filter(|node| node.0 == "comment").collect();
    assert_eq!(comments.len(), 1);
    assert_eq!(&source[comments[0].1..comments[0].2], b"# comment\0");
    for expected in [
        b"<#base>".as_slice(),
        b"<##namespace>",
        b"<#subject>",
        b"<##predicate>",
        b"<###object>",
        b"<#tail>",
        b"<#p>",
        b"<#type>",
        b"<#>",
        b"<>",
        b"<##>",
        b"<x#anchor>",
    ] {
        assert!(
            nodes
                .iter()
                .any(|node| node.0 == "iri_reference" && &source[node.1..node.2] == expected)
        );
    }
}

#[test]
fn raw_nul_preserves_all_quoted_forms_escapes_and_following_statements() {
    for quote in ["'", "\"", "'''", "\"\"\""] {
        for body in [
            "\0",
            "#before\0#after",
            "\0\x01\x02\x03\x04\x05\x06\x07\x08\t\x0b\x0c\x0e\x0f\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f\x7f",
            "\\t\\'\\\"\\\\\\u0000\\U00000000\0",
        ] {
            let literal = format!("{quote}{body}{quote}");
            let source = format!("<s> <p> {literal} .\n<tail> <p> <o> .\n");
            let tree = parser().parse(&source, None).unwrap();
            clean(source.as_bytes(), &tree);
            let nodes = shape(&tree);
            assert_eq!(nodes.iter().filter(|node| node.0 == "triple").count(), 2);
            assert_eq!(nodes.iter().filter(|node| node.0 == "comment").count(), 0);
            let string = nodes.iter().find(|node| node.0 == "string").unwrap();
            assert_eq!(&source.as_bytes()[string.1..string.2], literal.as_bytes());
            assert_eq!(
                nodes.iter().filter(|node| node.0 == "echar").count(),
                if body.starts_with('\\') { 4 } else { 0 },
            );
        }
    }
}

#[test]
fn quote_boundaries_keep_hashes_inside_strings_and_nul_comments_outside() {
    for (quote, body) in [
        ("'", ""),
        ("\"", ""),
        ("'", "#\0#"),
        ("\"", "#\0#"),
        ("'''", "'#\0''#\nend"),
        ("\"\"\"", "\"#\0\"\"#\nend"),
    ] {
        let literal = format!("{quote}{body}{quote}");
        for newline in ["\n", "\r\n"] {
            let source = format!("<s> <p> {literal} .# before\0after{newline}<tail> <p> <o> .");
            let tree = parser().parse(&source, None).unwrap();
            clean(source.as_bytes(), &tree);
            let nodes = shape(&tree);
            assert_eq!(nodes.iter().filter(|node| node.0 == "triple").count(), 2);
            let comments: Vec<_> = nodes.iter().filter(|node| node.0 == "comment").collect();
            assert_eq!(comments.len(), 1);
            let expected = if newline == "\r\n" {
                "# before\0after\r"
            } else {
                "# before\0after"
            };
            assert_eq!(
                &source.as_bytes()[comments[0].1..comments[0].2],
                expected.as_bytes()
            );
            let string = nodes.iter().find(|node| node.0 == "string").unwrap();
            assert_eq!(&source.as_bytes()[string.1..string.2], literal.as_bytes());
        }
    }
    let source = b"# before\0after\r";
    let tree = parser().parse(source, None).unwrap();
    clean(source, &tree);
    let nodes = shape(&tree);
    let comment = nodes.iter().find(|node| node.0 == "comment").unwrap();
    assert_eq!((comment.1, comment.2), (0, source.len()));
}

#[test]
fn trig_graphs_keep_raw_nul_and_multiline_strings_in_the_complete_document() {
    for (quote, body) in [
        ("'", "#before\0#after"),
        ("\"", "#before\0#after"),
        ("'''", "first\n#before\0#after\r\nlast"),
        ("\"\"\"", "first\n#before\0#after\r\nlast"),
    ] {
        let literal = format!("{quote}{body}{quote}");
        let source = format!(
            "GRAPH <g> {{ <s> <p> {literal} .\n# graph\0comment\n<second> <p> <o> . }}\n<tail> <p> <o> ."
        );
        let tree = parser().parse(&source, None).unwrap();
        clean(source.as_bytes(), &tree);
        let nodes = shape(&tree);
        assert_eq!(nodes.iter().filter(|node| node.0 == "graph").count(), 1);
        assert_eq!(nodes.iter().filter(|node| node.0 == "triple").count(), 3);
        assert_eq!(nodes.iter().filter(|node| node.0 == "comment").count(), 1);
        let string = nodes.iter().find(|node| node.0 == "string").unwrap();
        assert_eq!(&source.as_bytes()[string.1..string.2], literal.as_bytes());
    }
}

#[test]
fn native_nul_lexing_refuses_invalid_positions_escapes_and_unterminated_strings() {
    for invalid in [
        "\0<s> <p> <o> .",
        "<s> <p> <o> .\0<tail> <p> <o> .",
        "<s\0> <p> <o> .",
        "<s> <p> <o\0> .",
        "<s> <p> <#\0> .",
        "<s> <p> <# #invalid> .",
        "<s> <p> <#\ninvalid> .",
        "@prefix ex: <https://example.test/> . ex:s\0 ex:p ex:o .",
        "<s> <p> \"\0\\q\" .",
        "<s> <p> '\0\\u000x' .",
        "<s> <p> \"\0\\U0000000x\" .",
        "<s> <p> '\0\n' .",
        "<s> <p> \"\0\r\" .",
        "<s> <p> '\n\0x' .",
        "<s> <p> \"\r\0x\" .",
        "<s> <p> '\0\nx' .",
        "<s> <p> \"\0\rx\" .",
        "<s> <p> '\0\n\\t' .",
        "<s> <p> \"\0\r\\t\" .",
        "<s> <p> '\0\n\\u0041' .",
        "<s> <p> \"\0\r\\U00000041\" .",
        "<s> <p> 'line\nbreak' .",
        "<s> <p> \"line\rbreak\" .",
        "<s> <p> '\0",
        "<s> <p> \"\0",
        "<s> <p> '''first\n\0second",
        "<s> <p> \"\"\"first\n\0second",
        "GRAPH <g> { <s> <p> '''first\n\0second''' .",
    ] {
        let tree = parser().parse(invalid, None).unwrap();
        assert_eq!(tree.root_node().end_byte(), invalid.len());
        assert!(
            tree.root_node().has_error(),
            "{invalid:?}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn nul_edits_in_quoted_bodies_and_comments_match_fresh_native_parses() {
    for (quote, body) in [
        ("'", "#bodyx#"),
        ("\"", "#bodyx#"),
        ("'''", "'#bodyx''#\nlast"),
        ("\"\"\"", "\"#bodyx\"\"#\nlast"),
    ] {
        let source = format!("<s> <p> {quote}{body}{quote} .# commentx\r\n<tail> <p> <o> .");
        for marker in [b"bodyx".as_slice(), b"commentx".as_slice()] {
            let mut tree = parser().parse(&source, None).unwrap();
            clean(source.as_bytes(), &tree);
            let mut changed = source.as_bytes().to_vec();
            let start = changed
                .windows(marker.len())
                .position(|part| part == marker)
                .unwrap()
                + marker.len()
                - 1;
            let row = changed[..start]
                .iter()
                .filter(|byte| **byte == b'\n')
                .count();
            let column = start
                - changed[..start]
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map_or(0, |at| at + 1);
            for replacement in [0, b'x'] {
                changed[start] = replacement;
                tree.edit(&InputEdit {
                    start_byte: start,
                    old_end_byte: start + 1,
                    new_end_byte: start + 1,
                    start_position: Point::new(row, column),
                    old_end_position: Point::new(row, column + 1),
                    new_end_position: Point::new(row, column + 1),
                });
                let incremental = parser().parse(&changed, Some(&tree)).unwrap();
                let fresh = parser().parse(&changed, None).unwrap();
                clean(&changed, &incremental);
                clean(&changed, &fresh);
                assert_eq!(shape(&incremental), shape(&fresh));
                tree = incremental;
            }
        }
    }
}
