//! Qualifies YAML source boundaries independently of structural lowering.
//! Scanner positions must not change the tree when valid layout grows.

use tree_sitter::{InputEdit, Node, Parser, Point};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .expect("compatible grammar");
    parser
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            result.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    result.sort_by_key(Node::start_byte);
    result
}

#[test]
fn yaml_layout_positions_remain_exact_above_signed_sixteen_bits() {
    for width in [32_766, 32_767, 32_768, 40_000, 65_536] {
        for source in [
            format!("root:\n{}child: value\ntail: done\n", " ".repeat(width)),
            format!("first: value\n{}last: done\n", "\n".repeat(width)),
        ] {
            let tree = parser().parse(&source, None).expect("parse completes");
            assert!(
                !tree.root_node().has_error(),
                "width {width}: {}",
                tree.root_node().to_sexp()
            );
            let pairs = nodes(tree.root_node(), "block_mapping_pair");
            assert_eq!(pairs.len(), if source.starts_with("root") { 3 } else { 2 });
            let last = pairs.last().unwrap();
            assert!(last.utf8_text(source.as_bytes()).unwrap().ends_with("done"));
            assert_eq!(last.end_byte(), source.len() - 1);
        }
    }
}

#[test]
fn yaml_documents_anchors_aliases_and_complex_keys_keep_raw_spelling() {
    let source = "%YAML 1.2\n---\nbase: &template {name: '🌍', enabled: true}\ncopy: *template\n? [one, two]\n: !!str value\n...\n---\n- first\n- {second: 2}\n";
    let tree = parser().parse(source, None).unwrap();
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(nodes(root, "document").len(), 2);
    for (kind, expected) in [
        ("anchor", "&template"),
        ("alias", "*template"),
        ("tag", "!!str"),
    ] {
        let matched = nodes(root, kind);
        assert_eq!(matched.len(), 1, "{kind}");
        assert_eq!(matched[0].utf8_text(source.as_bytes()).unwrap(), expected);
    }
    for pair in nodes(root, "block_mapping_pair")
        .into_iter()
        .chain(nodes(root, "flow_pair"))
    {
        assert!(source.get(pair.byte_range()).is_some());
    }
}

#[test]
fn yaml_scalar_styles_preserve_source_without_claiming_schema_validation() {
    for value in [
        "plain text",
        "'single '' quote'",
        "\"escaped \\u00e9\"",
        "|-\n  literal\n  text",
        ">+\n  folded\n  text",
    ] {
        let source = format!("key: {value}\nnext: done\n");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source:?}: {}",
            tree.root_node().to_sexp()
        );
        assert_eq!(nodes(tree.root_node(), "block_mapping_pair").len(), 2);
    }
    // Parsing is not alias resolution, duplicate-key validation or scalar construction.
    for source in [
        "key: 1\nkey: 2\n",
        "missing: *absent\n",
        "cycle: &self {child: *self}\n",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(!tree.root_node().has_error(), "{source:?}");
    }
}

#[test]
fn yaml_block_scalar_nodes_omit_value_significant_trailing_lines() {
    for line_break in ["\n", "\r\n"] {
        for style in ["|+", ">+"] {
            let mut node_spellings = Vec::new();
            for trailing in [1, 3] {
                let source = format!(
                    "first: {style}{line_break}  alpha{}next: done{line_break}",
                    line_break.repeat(trailing)
                );
                let tree = parser().parse(&source, None).unwrap();
                let root = tree.root_node();
                assert!(!root.has_error(), "{source:?}");
                let scalars = nodes(root, "block_scalar");
                assert_eq!(scalars.len(), 1);
                let scalar = scalars[0];
                let spelling = scalar.utf8_text(source.as_bytes()).unwrap();
                assert_eq!(spelling, format!("{style}{line_break}  alpha"));
                let next = source.find("next:").unwrap();
                assert_eq!(
                    &source[scalar.end_byte()..next],
                    line_break.repeat(trailing)
                );
                node_spellings.push(spelling.to_owned());
            }
            // Keep-chomping values differ, although the native node text agrees.
            // Lowering must account for source beyond this capture before decoding.
            assert_eq!(node_spellings[0], node_spellings[1]);
        }
    }
}

#[test]
fn yaml_root_block_values_stop_before_both_document_markers() {
    for style in ["|+", ">+"] {
        for marker in ["---", "...", "--- # next", "...\t# end"] {
            let source = format!("{style}\nfirst\nsecond\n\n{marker}\n");
            let tree = parser().parse(&source, None).unwrap();
            let root = tree.root_node();
            assert!(!root.has_error(), "{source:?}: {}", root.to_sexp());
            let scalars = nodes(root, "block_scalar");
            assert_eq!(scalars.len(), 1);
            assert_eq!(
                scalars[0].utf8_text(source.as_bytes()).unwrap(),
                format!("{style}\nfirst\nsecond")
            );
            assert_eq!(
                nodes(root, "document").len(),
                if marker.starts_with("---") { 2 } else { 1 }
            );
        }
    }
}

#[test]
fn yaml_empty_blocks_exclude_dedented_trailing_comments() {
    for chomp in ["", "-", "+"] {
        let source = format!("empty: |{chomp}\n    \n   # trailing comment\nnext: done\n");
        let tree = parser().parse(&source, None).unwrap();
        let root = tree.root_node();
        assert!(!root.has_error(), "{source:?}: {}", root.to_sexp());
        let scalars = nodes(root, "block_scalar");
        assert_eq!(scalars.len(), 1);
        assert_eq!(
            scalars[0].utf8_text(source.as_bytes()).unwrap(),
            format!("|{chomp}")
        );
        let comments = nodes(root, "comment");
        assert_eq!(comments.len(), 1);
        assert!(comments[0].start_byte() > scalars[0].end_byte());
    }
}

#[test]
fn yaml_state_capacity_reports_errors_instead_of_truncating_indentation() {
    let mut parser = parser();
    for depth in [1, 128, 199, 200, 201, 260] {
        let mut source = String::new();
        for level in 0..depth {
            source.push_str(&" ".repeat(level));
            source.push_str(if level + 1 == depth {
                "key: leaf\n"
            } else {
                "key:\n"
            });
        }
        source.push_str("tail: done\n");
        let tree = parser.parse(&source, None).unwrap();
        assert_eq!(tree.root_node().has_error(), depth > 200, "depth {depth}");
        let clean = parser.parse("key: value\n", None).unwrap();
        assert!(!clean.root_node().has_error());
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    kind: String,
    range: std::ops::Range<usize>,
    start: Point,
    end: Point,
    flags: (bool, bool, bool),
    children: usize,
}

fn fingerprint(root: Node<'_>) -> Vec<Stamp> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        result.push(Stamp {
            kind: node.kind().to_owned(),
            range: node.byte_range(),
            start: node.start_position(),
            end: node.end_position(),
            flags: (node.is_named(), node.is_missing(), node.has_error()),
            children: node.child_count(),
        });
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result
}

fn point(source: &str) -> Point {
    let row = source.bytes().filter(|byte| *byte == b'\n').count();
    let column = source.rsplit('\n').next().unwrap().len();
    Point::new(row, column)
}

#[test]
fn yaml_incremental_layout_and_scalar_edits_match_every_fresh_node() {
    for source in [
        "root:\n  child: value\n  list:\n  - one\n  - two\ntail: done\n",
        "base: &data {key: '🌍'}\ncopy: *data\nmerge: {<<: *data}\n",
        "? [one, two]\n: {value: 3}\n? {key: 1}\n: other\n",
        "literal: |2-\n  first\n\n  second\nfolded: >+\n  text\n  more\n",
        "%YAML 1.2\n---\nkey: value\n...\n---\nkey: !!str true\n",
        "quotes: [\"escaped \\u00e9\", 'single '' quote']\r\nnext: null\r\n",
        "- - nested\n  - sequence\n- &anchor\n  - *anchor\n",
    ] {
        let mut parser = parser();
        let original = parser.parse(source, None).unwrap();
        assert!(!original.root_node().has_error(), "{source:?}");
        let boundaries: Vec<_> = source
            .char_indices()
            .map(|(offset, _)| offset)
            .chain([source.len()])
            .collect();
        for (index, &start) in boundaries.iter().enumerate() {
            let end = boundaries.get(index + 1).copied().unwrap_or(start);
            for replacement in ["", " ", "\n", "🌍"] {
                let updated = format!("{}{replacement}{}", &source[..start], &source[end..]);
                let mut previous = original.clone();
                previous.edit(&InputEdit {
                    start_byte: start,
                    old_end_byte: end,
                    new_end_byte: start + replacement.len(),
                    start_position: point(&source[..start]),
                    old_end_position: point(&source[..end]),
                    new_end_position: point(&updated[..start + replacement.len()]),
                });
                let incremental = parser.parse(&updated, Some(&previous)).unwrap();
                let fresh = parser.parse(&updated, None).unwrap();
                assert_eq!(
                    fingerprint(incremental.root_node()),
                    fingerprint(fresh.root_node()),
                    "{source:?}: replace {start}..{end} with {replacement:?}"
                );
            }
        }
    }
}

#[test]
fn yaml_incremental_wide_positions_match_fresh_trees() {
    for padding in [" ".repeat(65_536), "\n".repeat(65_536)] {
        let source = format!("root:\n{padding}child: value\ntail: done\n");
        let start = source.find("value").unwrap();
        let end = start + "value".len();
        let mut parser = parser();
        let original = parser.parse(&source, None).unwrap();
        assert!(!original.root_node().has_error());
        for replacement in ["🌍", "{nested: [one, two]}", "|\n  text"] {
            let updated = format!("{}{replacement}{}", &source[..start], &source[end..]);
            let mut previous = original.clone();
            previous.edit(&InputEdit {
                start_byte: start,
                old_end_byte: end,
                new_end_byte: start + replacement.len(),
                start_position: point(&source[..start]),
                old_end_position: point(&source[..end]),
                new_end_position: point(&updated[..start + replacement.len()]),
            });
            let incremental = parser.parse(&updated, Some(&previous)).unwrap();
            let fresh = parser.parse(&updated, None).unwrap();
            assert_eq!(
                fingerprint(incremental.root_node()),
                fingerprint(fresh.root_node())
            );
        }
    }
}

#[test]
fn yaml_directive_presence_checks_do_not_wrap_at_unsigned_sixteen_bits() {
    let digits = "0".repeat(65_536);
    let handle = "a".repeat(65_536);
    for source in [
        format!("%YAML {digits}.2\n---\nkey: value\n"),
        format!("%YAML 1.{digits}\n---\nkey: value\n"),
        format!("%TAG !{handle}! tag:example.org,2000:\n---\nkey: value\n"),
    ] {
        // Native syntax preserves spelling; supported YAML versions are a separate policy.
        let tree = parser().parse(&source, None).unwrap();
        assert!(!tree.root_node().has_error());
        assert_eq!(tree.root_node().end_byte(), source.len());
    }
}
