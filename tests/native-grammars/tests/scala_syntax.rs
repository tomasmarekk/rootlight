//! Scala syntax qualification before production adapter registration.
//! Source spans and incremental trees are checked independently of JVM semantics.

use tree_sitter::{InputEdit, Node, Parser, Point};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_scala::LANGUAGE.into())
        .unwrap();
    parser
}

fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        result.push(node);
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind_id()));
    result
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

#[test]
fn scala_braced_and_indented_declarations_keep_written_names() {
    for source in [
        "package sample\ntrait Reader { def read(value: Int): Int }\ncase class Entry(value: Int)\nobject Store { type Key = String; val limit = 4; def twice(value: Int): Int = value * 2 }\n",
        "package sample\ntrait Reader:\n  def read(value: Int): Int\ncase class Entry(value: Int)\nobject Store:\n  type Key = String\n  val limit = 4\n  def twice(value: Int): Int = value * 2\n",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(tree.root_node().byte_range(), 0..source.len());
        let all = nodes(tree.root_node());
        for (kind, field, expected) in [
            ("trait_definition", "name", "Reader"),
            ("class_definition", "name", "Entry"),
            ("object_definition", "name", "Store"),
            ("type_definition", "name", "Key"),
            ("val_definition", "pattern", "limit"),
            ("function_definition", "name", "twice"),
        ] {
            let found: Vec<_> = all
                .iter()
                .filter(|node| {
                    node.kind() == kind
                        && node.child_by_field_name(field).is_some_and(|name| {
                            name.utf8_text(source.as_bytes()).unwrap() == expected
                        })
                })
                .collect();
            assert_eq!(found.len(), 1, "{kind}: {expected}");
        }
        for node in all {
            assert!(source.get(node.byte_range()).is_some());
        }
    }
}

#[test]
fn scala_three_enum_given_extension_and_opaque_type_remain_distinct() {
    let source = "enum Color:\n  case Red, Blue\n  case Custom(value: Int)\nopaque type Meter = Double\ngiven ordering: Ordering[Int] = Ordering.Int\nextension (value: Int)\n  def doubled: Int = value * 2\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let all = nodes(tree.root_node());
    for (kind, expected) in [
        ("enum_definition", "Color"),
        ("full_enum_case", "Custom"),
        ("type_definition", "Meter"),
        ("given_definition", "ordering"),
        ("function_definition", "doubled"),
    ] {
        assert!(
            all.iter().any(|node| {
                node.kind() == kind
                    && node
                        .child_by_field_name("name")
                        .is_some_and(|name| name.utf8_text(source.as_bytes()).unwrap() == expected)
            }),
            "{kind}: {expected}"
        );
    }
    assert_eq!(
        all.iter()
            .filter(|node| node.kind() == "extension_definition")
            .count(),
        1
    );
}

#[test]
fn scala_layout_and_interpolated_unicode_edits_equal_fresh_trees() {
    let mut source = "object Store:\n  def show(value: Int): String =\n    val text = s\"雪:${value}\"\n    if value > 0 then\n      text\n    else\n      raw\"empty\"\n  val count = 1\n".to_owned();
    let mut parser = parser();
    let mut tree = parser.parse(&source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    for (old, new) in [
        ("雪", "δ"),
        ("δ", "😀"),
        ("value > 0", "value > 20"),
        ("      text", "      text + \"!\""),
        ("raw\"empty\"", "raw\"none\""),
        ("  val count = 1", "  // nested /* text */\n  val count = 3"),
        ("Store", "Catalog"),
    ] {
        let start = source.find(old).unwrap();
        let end = start + old.len();
        let changed = source.replacen(old, new, 1);
        tree.edit(&InputEdit {
            start_byte: start,
            old_end_byte: end,
            new_end_byte: start + new.len(),
            start_position: point(&source, start),
            old_end_position: point(&source, end),
            new_end_position: point(&changed, start + new.len()),
        });
        let incremental = parser.parse(&changed, Some(&tree)).unwrap();
        let fresh = self::parser().parse(&changed, None).unwrap();
        assert!(
            !fresh.root_node().has_error(),
            "{}",
            fresh.root_node().to_sexp()
        );
        assert_eq!(
            incremental.root_node().to_sexp(),
            fresh.root_node().to_sexp()
        );
        let shape = |root| {
            nodes(root)
                .into_iter()
                .map(|node| {
                    (
                        node.kind_id(),
                        node.byte_range(),
                        node.start_position(),
                        node.end_position(),
                        node.is_missing(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(incremental.root_node()), shape(fresh.root_node()));
        source = changed;
        tree = incremental;
    }
}

#[test]
fn scala_comments_strings_and_recovery_preserve_source_boundaries() {
    let source = "object Real { /* class Fake { /* def nested = 1 */ } */ val text = \"def imagined = 2\"; def actual = 3 }";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let names: Vec<_> = nodes(tree.root_node())
        .into_iter()
        .filter(|node| {
            matches!(
                node.kind(),
                "class_definition" | "object_definition" | "function_definition"
            )
        })
        .map(|node| {
            node.child_by_field_name("name")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap()
        })
        .collect();
    assert_eq!(names, ["Real", "actual"]);
    for malformed in [
        "object Broken { def run(",
        "object Broken:\n  val value = \"unfinished",
        "enum Broken:\n  case Value(",
    ] {
        let tree = parser().parse(malformed, None).unwrap();
        assert!(tree.root_node().has_error(), "{malformed}");
        for node in nodes(tree.root_node()) {
            assert!(malformed.get(node.byte_range()).is_some());
        }
    }
}

#[test]
fn scala_wide_indentation_keeps_definitions_in_their_written_owner() {
    for width in [2, 16_383, 16_384, 32_768, 65_536] {
        let source = format!(
            "object Outer:\n{indent}def value =\n{indent}  1\nobject After {{ def next = 2 }}\n",
            indent = " ".repeat(width)
        );
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "width {width}: {}",
            tree.root_node().to_sexp()
        );
        let all = nodes(tree.root_node());
        for (function, owner) in [("value", "Outer"), ("next", "After")] {
            let declarations: Vec<_> = all
                .iter()
                .filter(|node| {
                    node.kind() == "function_definition"
                        && node.child_by_field_name("name").is_some_and(|name| {
                            name.utf8_text(source.as_bytes()).unwrap() == function
                        })
                })
                .collect();
            assert_eq!(declarations.len(), 1, "width {width}: {function}");
            let mut ancestor = declarations[0].parent().unwrap();
            while ancestor.kind() != "object_definition" {
                ancestor = ancestor.parent().expect("function has an object owner");
            }
            assert_eq!(
                ancestor
                    .child_by_field_name("name")
                    .unwrap()
                    .utf8_text(source.as_bytes())
                    .unwrap(),
                owner,
                "width {width}: {function}"
            );
        }
    }
}

#[test]
fn scala_layout_capacity_is_a_parse_error_not_a_truncated_success() {
    let mut parser = parser();
    for depth in [1, 128, 200, 201, 202, 260] {
        let mut source = String::new();
        for level in 0..depth {
            source.push_str(&format!("{}object Scope{level}:\n", " ".repeat(level)));
        }
        source.push_str(&format!(
            "{}def value = 1\nobject After {{}}\n",
            " ".repeat(depth)
        ));
        let tree = parser.parse(&source, None).unwrap();
        assert_eq!(tree.root_node().has_error(), depth > 201, "depth {depth}");
        for node in nodes(tree.root_node()) {
            assert!(source.get(node.byte_range()).is_some());
        }
        let clean = parser
            .parse("object Clean { def value = 1 }", None)
            .unwrap();
        assert!(!clean.root_node().has_error());
    }
}

#[test]
fn scala_wide_layout_and_blank_line_edits_match_every_fresh_node() {
    for (width, blank_lines) in [(16_384, 1), (32_768, 32_768), (65_536, 65_536)] {
        let source = format!(
            "object Outer:\n{indent}def value =\n{indent}  1{blank}{indent}def next = 2\nobject After {{}}\n",
            indent = " ".repeat(width),
            blank = "\n".repeat(blank_lines)
        );
        let mut parser = parser();
        let mut original = parser.parse(&source, None).unwrap();
        assert!(
            !original.root_node().has_error(),
            "width {width}, blank lines {blank_lines}"
        );
        let start = source.find("1\n").unwrap();
        let changed = source.replacen("1\n", "100\n", 1);
        original.edit(&InputEdit {
            start_byte: start,
            old_end_byte: start + 1,
            new_end_byte: start + 3,
            start_position: point(&source, start),
            old_end_position: point(&source, start + 1),
            new_end_position: point(&changed, start + 3),
        });
        let incremental = parser.parse(&changed, Some(&original)).unwrap();
        let fresh = self::parser().parse(&changed, None).unwrap();
        assert!(!fresh.root_node().has_error());
        let incremental_nodes = nodes(incremental.root_node());
        let fresh_nodes = nodes(fresh.root_node());
        assert_eq!(incremental_nodes.len(), fresh_nodes.len());
        for (incremental, fresh) in incremental_nodes.into_iter().zip(fresh_nodes) {
            assert_eq!(incremental.kind(), fresh.kind());
            assert_eq!(incremental.byte_range(), fresh.byte_range());
            assert_eq!(incremental.start_position(), fresh.start_position());
            assert_eq!(incremental.end_position(), fresh.end_position());
            assert_eq!(incremental.is_missing(), fresh.is_missing());
            assert_eq!(incremental.has_error(), fresh.has_error());
        }
    }
}
