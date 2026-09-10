//! Nix syntax admission compares native source structure with an independent parser.
//! Neither parser evaluates paths, imports, builtins or package expressions.

use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

const SOURCE: &str = include_str!("../../fixtures/nix/expressions.nix");

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_nix::LANGUAGE.into())
        .unwrap();
    parser
}

fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut nodes = Vec::new();
    while let Some(node) = pending.pop() {
        nodes.push(node);
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    nodes.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind_id()));
    nodes
}

fn signature(tree: &Tree) -> Vec<(String, std::ops::Range<usize>, bool, bool)> {
    nodes(tree.root_node())
        .into_iter()
        .map(|node| {
            (
                node.kind().to_owned(),
                node.byte_range(),
                node.is_named(),
                node.is_missing(),
            )
        })
        .collect()
}

fn point(source: &str, offset: usize) -> Point {
    let prefix = source.get(..offset).unwrap();
    Point::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count(),
        prefix
            .rfind('\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn independently_valid(source: &str) {
    let parsed = rnix::Root::parse(source);
    assert!(
        parsed.errors().is_empty(),
        "{source}: {:?}",
        parsed.errors()
    );
    assert_eq!(parsed.syntax().to_string(), source);
}

#[test]
fn authored_bindings_formals_and_paths_have_exact_source_ranges() {
    independently_valid(SOURCE);
    let language: tree_sitter::Language = tree_sitter_nix::LANGUAGE.into();
    assert_eq!(language.abi_version(), 14);
    let tree = parser().parse(SOURCE, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().byte_range(), 0..SOURCE.len());
    let all = nodes(tree.root_node());
    assert!(
        all.iter()
            .all(|node| SOURCE.get(node.byte_range()).is_some())
    );
    let bindings: Vec<_> = all
        .iter()
        .filter(|node| node.kind() == "binding")
        .map(|node| {
            node.child_by_field_name("attrpath")
                .unwrap()
                .utf8_text(SOURCE.as_bytes())
                .unwrap()
        })
        .collect();
    assert_eq!(
        bindings,
        [
            "choose",
            "nested",
            "first",
            "second",
            "package.name",
            "package.run",
            "\"quoted.key\"",
            "\"${choose \"dynamic\"}\"",
            "path",
            "search",
            "home",
            "values",
            "script",
            "enabled"
        ]
    );
    let formals: Vec<_> = all
        .iter()
        .filter(|node| node.kind() == "formal")
        .map(|node| {
            node.child_by_field_name("name")
                .unwrap()
                .utf8_text(SOURCE.as_bytes())
                .unwrap()
        })
        .collect();
    assert_eq!(formals, ["lib", "system"]);
    for kind in [
        "inherit",
        "inherit_from",
        "path_expression",
        "hpath_expression",
        "spath_expression",
    ] {
        assert_eq!(
            all.iter()
                .filter(|node| node.is_named() && node.kind() == kind)
                .count(),
            1,
            "{kind}"
        );
    }
}

#[test]
fn empty_inheritance_is_valid_without_inventing_attribute_names() {
    for source in [
        "{ inherit; }",
        "{ inherit (builtins); }",
        "let inherit; in 1",
        "let inherit ({}); in 1",
    ] {
        independently_valid(source);
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        assert!(
            !nodes(tree.root_node())
                .iter()
                .any(|node| node.kind() == "inherited_attrs")
        );
    }
}

#[test]
fn malformed_sources_retain_errors_without_out_of_bounds_nodes() {
    for source in [
        "{ value = ; }",
        "{ inherit (); }",
        "{ value = \"unfinished",
        "let first = 1;",
        "x: if x then",
        "{ value = ''unfinished",
    ] {
        assert!(!rnix::Root::parse(source).errors().is_empty(), "{source}");
        let tree = parser().parse(source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        assert!(
            nodes(tree.root_node())
                .iter()
                .all(|node| source.get(node.byte_range()).is_some())
        );
    }
}

#[test]
fn edits_inside_strings_paths_and_bindings_match_fresh_syntax() {
    for (before, after) in [
        ("demo", "expanded"),
        ("first = 1", "first = 22"),
        ("modules/", "modules/sub/"),
        ("λ😀", "δ🌍\ncontinued"),
    ] {
        let mut parser = parser();
        let mut old = parser.parse(SOURCE, None).unwrap();
        let start = SOURCE.find(before).unwrap();
        let mut changed = SOURCE.to_owned();
        changed.replace_range(start..start + before.len(), after);
        independently_valid(&changed);
        old.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + before.len(),
            new_end_byte: start + after.len(),
            start_position: point(SOURCE, start),
            old_end_position: point(SOURCE, start + before.len()),
            new_end_position: point(&changed, start + after.len()),
        });
        let incremental = parser.parse(&changed, Some(&old)).unwrap();
        let fresh = parser.parse(&changed, None).unwrap();
        assert!(!incremental.root_node().has_error());
        assert_eq!(signature(&incremental), signature(&fresh), "{before}");
    }
}

#[test]
fn scanner_reuse_after_malformed_delimiters_preserves_valid_source() {
    let mut reused = parser();
    let expected = parser().parse(SOURCE, None).unwrap();
    for source in [
        "\"${",
        "''${",
        "./path/${",
        "{ a = \"\\",
        "# hidden\n{ broken = ; }",
        "\0",
    ] {
        let _ = reused.parse(source, None).unwrap();
        let result = reused.parse(SOURCE, None).unwrap();
        assert_eq!(signature(&result), signature(&expected), "{source}");
    }
}

#[test]
fn strings_and_comments_cannot_invent_bindings() {
    for source in [
        r#"{ actual = "fake = 1; ${"still fake = 2;"}"; }"#,
        "{ /* hidden = 2; */ actual = ''other = 3; ''${literal}''; # absent = 4;\n}",
    ] {
        independently_valid(source);
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let names: Vec<_> = nodes(tree.root_node())
            .into_iter()
            .filter(|node| node.kind() == "binding")
            .map(|node| {
                node.child_by_field_name("attrpath")
                    .unwrap()
                    .utf8_text(source.as_bytes())
                    .unwrap()
            })
            .collect();
        assert_eq!(names, ["actual"]);
    }
}

#[test]
fn included_source_keeps_host_coordinates_without_parsing_host_text() {
    let prefix = "# Host λ😀\r\n```nix\r\n";
    let source = format!("{prefix}{SOURCE}\r\n```\r\nUnrelated host text");
    let start = prefix.len();
    let end = start + SOURCE.len();
    let mut parser = parser();
    parser
        .set_included_ranges(&[tree_sitter::Range {
            start_byte: start,
            end_byte: end,
            start_point: point(&source, start),
            end_point: point(&source, end),
        }])
        .unwrap();
    let tree = parser.parse(&source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert!(nodes(tree.root_node()).iter().all(|node| {
        node.start_byte() >= start
            && node.end_byte() <= end
            && source.get(node.byte_range()).is_some()
    }));
    assert_eq!(tree.root_node().byte_range(), start..end);
}
