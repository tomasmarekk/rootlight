//! Preserve physical Dgraph term fields without assigning RDF IRI or graph meaning.
//! Native lexical controls include byte escapes, NUL data and parser recovery.

use std::{fs, path::PathBuf};

use tree_sitter::{InputEdit, Parser, Point, Tree};

type Shape = (String, usize, usize, bool, bool, bool);

fn parser() -> Parser {
    let language: tree_sitter::Language = rootlight_tree_sitter_nquads_dgraph::LANGUAGE.into();
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
    // The runtime consumes only a leading UTF-8 BOM before lexing. Following
    // node positions remain physical byte offsets rather than being rebased.
    let encoding_prefix = usize::from(source.starts_with(b"\xef\xbb\xbf")) * 3;
    assert_eq!(tree.root_node().byte_range(), encoding_prefix..source.len());
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert!(shape(tree).iter().all(|node| !node.4 && !node.5));
}

fn roles(source: &[u8], tree: &Tree) -> Vec<[String; 3]> {
    let mut cursor = tree.root_node().walk();
    tree.root_node()
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "quad")
        .map(|quad| {
            assert!(!quad.has_error());
            assert!(quad.child_by_field_name("graph").is_none());
            assert!(quad.child_by_field_name("namespace").is_none());
            ["subject", "predicate", "object"].map(|field| {
                let node = quad.child_by_field_name(field).unwrap();
                assert_eq!(node.parent().unwrap().id(), quad.id());
                assert!(quad.start_byte() <= node.start_byte());
                assert!(quad.end_byte() >= node.end_byte());
                if field != "object" || node.kind() == "identifier" {
                    assert_eq!(node.kind(), "identifier");
                } else {
                    assert_eq!(node.kind(), "default_value");
                    let value = node.child_by_field_name("value").unwrap();
                    assert_eq!(value.kind(), "string");
                    assert_eq!(value.parent().unwrap().id(), node.id());
                    assert_eq!(value.byte_range(), node.byte_range());
                }
                node.utf8_text(source).unwrap().to_owned()
            })
        })
        .collect()
}

// Apache-2.0 fixture from dgraph-io/dgraph at the pinned source revision.
// https://github.com/dgraph-io/dgraph/blob/c587cba75f60445125459b46b4d81757a73384aa/dgraph/cmd/alpha/testdata.nq
// The complete upstream notice is retained in LICENSE-DGRAPH.
const ORIGINAL: &[u8] = b"<alice> <follows> <bob> .\n<bob> <follows> <fred> .\n<bob> <status> \"I'm your Uncle Bob!\" .\n<charlie> <follows> <bob> .\n<charlie> <follows> <dani> .\n<dani> <follows> <bob> .\n<dani> <follows> <greg> .\n<dani> <status> \"Smart Dani\" .\n<emily> <follows> <fred> .\n<fred> <follows> <greg> .\n<greg> <status> \"Mr. Cool greg\" .\n";

#[test]
fn original_eleven_statements_have_exact_native_physical_roles() {
    assert_eq!(ORIGINAL.len(), 316);
    let tree = parse(ORIGINAL);
    clean(ORIGINAL, &tree);
    let actual = roles(ORIGINAL, &tree);
    let expected = [
        ["<alice>", "<follows>", "<bob>"],
        ["<bob>", "<follows>", "<fred>"],
        ["<bob>", "<status>", "\"I'm your Uncle Bob!\""],
        ["<charlie>", "<follows>", "<bob>"],
        ["<charlie>", "<follows>", "<dani>"],
        ["<dani>", "<follows>", "<bob>"],
        ["<dani>", "<follows>", "<greg>"],
        ["<dani>", "<status>", "\"Smart Dani\""],
        ["<emily>", "<follows>", "<fred>"],
        ["<fred>", "<follows>", "<greg>"],
        ["<greg>", "<status>", "\"Mr. Cool greg\""],
    ];
    assert_eq!(actual, expected.map(|row| row.map(str::to_owned)));
    assert!(shape(&tree).iter().all(|node| node.0 != "iri_reference"));
    let mut cursor = tree.root_node().walk();
    let quads: Vec<_> = tree.root_node().named_children(&mut cursor).collect();
    let mut offset = 0;
    for (quad, line) in quads
        .iter()
        .zip(ORIGINAL.split_inclusive(|byte| *byte == b'\n'))
    {
        assert_eq!(quad.byte_range(), offset..offset + line.len() - 1);
        offset += line.len();
    }
    assert_eq!(offset, ORIGINAL.len());
}

#[test]
fn relative_angle_uid_and_escaped_identifier_spellings_are_retained() {
    for identifier in [
        "alice",
        "0x01",
        "http://example.test/alice#anchor",
        "λ",
        "_:label",
        r"a\u0020b",
        r"a\UFFFFFFFF",
    ] {
        let value = format!("<{identifier}>");
        let source = format!("{value}<follows>{value}.");
        let tree = parse(source.as_bytes());
        clean(source.as_bytes(), &tree);
        assert_eq!(
            roles(source.as_bytes(), &tree),
            [[value.clone(), "<follows>".into(), value]]
        );
    }
}

#[test]
fn native_default_value_escapes_preserve_complete_string_tokens() {
    for value in [
        r#""""#,
        r#""\v\t\b\n\r\f\"\\""#,
        r#""\xFF\x00\xC3\xA9""#,
        r#""\u03BB\U0001F600""#,
        "\"λ😀 and raw\0value\t\"",
    ] {
        let source = format!("<alice><status>{value}.");
        let tree = parse(source.as_bytes());
        clean(source.as_bytes(), &tree);
        assert_eq!(
            roles(source.as_bytes(), &tree),
            [[
                "<alice>".to_owned(),
                "<status>".to_owned(),
                value.to_owned()
            ]]
        );
    }
}

#[test]
fn raw_nul_comments_eof_and_line_endings_do_not_truncate_source() {
    for prefix in ["", "\u{feff}"] {
        for ending in ["", "\n", "\r", "\r\n"] {
            let source =
                format!("{prefix}#\0 header\r\n<alice><status>\"#λ\0😀#\".#\0 tail{ending}");
            let tree = parse(source.as_bytes());
            clean(source.as_bytes(), &tree);
            assert_eq!(roles(source.as_bytes(), &tree).len(), 1);
            let string = shape(&tree)
                .into_iter()
                .find(|node| node.0 == "string")
                .unwrap();
            assert_eq!(
                &source.as_bytes()[string.1..string.2],
                "\"#λ\0😀#\"".as_bytes()
            );
        }
    }
    for source in [b"".as_slice(), b"#\0 comment at eof", b"\r\n#\0 comment\n"] {
        clean(source, &parse(source));
    }
    for source in [
        b"<alice><status>\"\0unterminated".as_slice(),
        b"<alice>\0<status><bob>.",
    ] {
        let tree = parse(source);
        assert_eq!(tree.root_node().end_byte(), source.len());
        assert!(tree.root_node().has_error());
    }
}

#[test]
fn bare_uids_invalid_identifier_escapes_and_invalid_literals_are_refused() {
    for source in [
        "0x01<follows><bob>.",
        "<alice><follows>0x01.",
        "<><follows><bob>.",
        "<a b><follows><bob>.",
        "<a\0b><follows><bob>.",
        r"<a\xFF><follows><bob>.",
        r"<a\u00XX><follows><bob>.",
        r#"<alice><status>"\a"."#,
        r#"<alice><status>"\'"."#,
        r#"<alice><status>"\01"."#,
        r#"<alice><status>"\x0"."#,
        r#"<alice><status>"\xGG"."#,
        r#"<alice><status>"\uD800"."#,
        r#"<alice><status>"\U00110000"."#,
        "<alice><status>\"raw\nline\".",
        "<alice><status>\"raw\rline\".",
        "<alice><follows><bob>",
        "<alice><follows>.",
        "<alice>\n<follows><bob>.",
        "<alice><follows><bob>. <tail><follows><bob>.",
    ] {
        let tree = parse(source.as_bytes());
        assert_eq!(tree.root_node().end_byte(), source.len());
        assert!(tree.root_node().has_error(), "{source:?}");
    }
}

#[test]
fn constructs_outside_the_bounded_family_are_never_reported_as_rdf_graphs() {
    for source in [
        "_:alice<follows><bob>.",
        "<alice><follows>_:bob.",
        "<alice><status>\"value\"@en.",
        "<alice><status>\"value\"^^<xs:string>.",
        "<alice><follows><bob><0x10>.",
        "<alice><follows><bob><10>.",
        "<alice><follows><bob>(friend=true).",
        "uid(a)<follows><bob>.",
        "<alice><follows>val(a).",
        "*<follows>*.",
    ] {
        let tree = parse(source.as_bytes());
        assert!(tree.root_node().has_error(), "{source}");
        assert!(shape(&tree).iter().all(|node| node.0 != "graph"));
    }
}

fn point(source: &[u8], byte: usize) -> Point {
    let prefix = &source[..byte];
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix.len()
            - prefix
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |index| index + 1),
    )
}

#[test]
fn incremental_nul_edits_and_recovery_preserve_native_role_boundaries() {
    let before = "# header\r\n<λ><status>\"first\0value\".\r\n<tail><follows><bob>.".as_bytes();
    let after = "# header\r\n<λ><status>\"changed\0value\".\r\n<tail><follows><bob>.".as_bytes();
    let start = before
        .windows(5)
        .position(|bytes| bytes == b"first")
        .unwrap();
    let mut old_tree = parse(before);
    old_tree.edit(&InputEdit {
        start_byte: start,
        old_end_byte: start + 5,
        new_end_byte: start + 7,
        start_position: point(before, start),
        old_end_position: point(before, start + 5),
        new_end_position: point(after, start + 7),
    });
    let incremental = parser().parse(after, Some(&old_tree)).unwrap();
    let fresh = parse(after);
    clean(after, &incremental);
    assert_eq!(shape(&incremental), shape(&fresh));
    assert_eq!(roles(after, &incremental), roles(after, &fresh));
    let recovered = b"<broken><follows>.\n<tail><follows><bob>.\n";
    let tree = parse(recovered);
    assert!(tree.root_node().has_error());
    assert_eq!(tree.root_node().end_byte(), recovered.len());
    let mut cursor = tree.root_node().walk();
    let tail = tree
        .root_node()
        .named_children(&mut cursor)
        .find(|node| node.kind() == "quad" && !node.has_error())
        .unwrap();
    assert_eq!(
        tail.child_by_field_name("subject")
            .unwrap()
            .utf8_text(recovered)
            .unwrap(),
        "<tail>"
    );
}

#[test]
#[ignore = "requires an explicitly supplied pinned original Dgraph corpus"]
fn pinned_original_dgraph_source_matches_the_portable_native_fixture() {
    let parent = PathBuf::from(
        std::env::var_os("ROOTLIGHT_NQUADS_CORPUS").expect("explicit pinned corpus parent"),
    );
    let source = fs::read(parent.join("187_NQuads_dgraph/dgraph/cmd/alpha/testdata.nq")).unwrap();
    assert_eq!(source, ORIGINAL);
    let tree = parse(&source);
    clean(&source, &tree);
    assert_eq!(roles(&source, &tree).len(), 11);
}
