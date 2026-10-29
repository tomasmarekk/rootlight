//! Native CST controls for the original Handlebars block-parameter witness.
//! Exact source shapes do not establish helper dispatch, matching block names,
//! lexical targets, or template evaluation; those remain adapter responsibilities.

use std::ops::Range;

use tree_sitter::{Node, Parser, Tree};

const ORIGINAL: &[u8] = include_bytes!(
    "../../../crates/rootlight-adapter-treesitter/tests/fixtures/handlebars/open-color.inkscape.hbs"
);
const SKETCH_PALETTE: &[u8] = include_bytes!(
    "../../../crates/rootlight-adapter-treesitter/tests/fixtures/handlebars/open-color.sketchpalette.hbs"
);
const OPENZEPPELIN_CONTRACT: &[u8] = include_bytes!(
    "../../../crates/rootlight-adapter-treesitter/tests/fixtures/handlebars/openzeppelin/contract.hbs"
);
const OPENZEPPELIN_PAGE: &[u8] = include_bytes!(
    "../../../crates/rootlight-adapter-treesitter/tests/fixtures/handlebars/openzeppelin/page.hbs"
);

fn parse(source: &[u8]) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_glimmer::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

fn nodes(tree: &Tree) -> Vec<Node<'_>> {
    let mut found = Vec::new();
    let mut cursor = tree.root_node().walk();
    loop {
        found.push(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return found;
            }
        }
    }
}

fn clean(source: &[u8]) -> Tree {
    let tree = parse(source);
    let root = tree.root_node();
    assert_eq!(root.kind(), "template");
    assert_eq!(root.byte_range(), 0..source.len());
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert!(
        nodes(&tree)
            .iter()
            .all(|node| !node.is_error() && !node.is_missing())
    );
    tree
}

fn identifier(tree: &Tree, span: Range<usize>) -> Node<'_> {
    let found: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == "identifier" && node.byte_range() == span)
        .collect();
    assert_eq!(found.len(), 1, "expected one identifier at {span:?}");
    found[0]
}

fn enclosing_block(mut node: Node<'_>) -> Node<'_> {
    loop {
        node = node.parent().unwrap();
        if node.kind() == "block_statement" {
            return node;
        }
    }
}

#[test]
fn original_template_preserves_exact_block_parameters_and_read_shapes() {
    assert_eq!(ORIGINAL.len(), 297);
    let tree = clean(ORIGINAL);
    let mut blocks = Vec::new();
    for (span, name, helper) in [
        (86..91, "color", "each"),
        (122..125, "hex", "each"),
        (165..168, "rgb", "with"),
    ] {
        let parameter = identifier(&tree, span);
        assert_eq!(parameter.utf8_text(ORIGINAL).unwrap(), name);
        let parameters = parameter.parent().unwrap();
        assert_eq!(parameters.kind(), "block_params");
        let start = parameters.parent().unwrap();
        assert_eq!(start.kind(), "block_statement_start");
        assert_eq!(
            start
                .child_by_field_name("path")
                .unwrap()
                .utf8_text(ORIGINAL)
                .unwrap(),
            helper
        );
        blocks.push(enclosing_block(start));
    }
    assert_eq!(blocks[1].parent(), Some(blocks[0]));
    assert_eq!(blocks[2].parent(), Some(blocks[1]));
    for (span, name, block) in [(247..252, "color", blocks[1]), (188..191, "rgb", blocks[2])] {
        let read = identifier(&tree, span);
        assert_eq!(read.utf8_text(ORIGINAL).unwrap(), name);
        let path = read.parent().unwrap();
        assert_eq!(path.kind(), "path_expression");
        assert_eq!(path.named_child(0), Some(read));
        assert_eq!(path.parent().unwrap().kind(), "mustache_statement");
        assert_eq!(enclosing_block(read), block);
    }
    let hex = identifier(&tree, 156..159);
    assert_eq!(hex.utf8_text(ORIGINAL).unwrap(), "hex");
    let helper = hex.parent().unwrap();
    assert_eq!(helper.kind(), "helper_invocation");
    assert_eq!(helper.child_by_field_name("argument"), Some(hex));
    assert_eq!(
        helper
            .child_by_field_name("helper")
            .unwrap()
            .utf8_text(ORIGINAL)
            .unwrap(),
        "hex2rgb"
    );
    assert_eq!(helper.parent().unwrap().kind(), "sub_expression");
    assert_eq!(enclosing_block(hex), blocks[2]);
}

#[test]
fn original_sketch_palette_preserves_the_parent_data_argument() {
    assert_eq!(SKETCH_PALETTE.len(), 372);
    let tree = clean(SKETCH_PALETTE);
    let parent_data: Vec<_> = nodes(&tree)
        .into_iter()
        .filter(|node| node.is_named() && node.byte_range() == (297..305))
        .collect();
    assert_eq!(parent_data.len(), 1);
    assert_eq!(parent_data[0].kind(), "data_path_expression");
    assert_eq!(
        parent_data[0].utf8_text(SKETCH_PALETTE).unwrap(),
        "@../last"
    );
    assert_eq!(parent_data[0].parent().unwrap().kind(), "helper_invocation");
    for (span, name) in [
        (78..83, "color"),
        (84..94, "colorIndex"),
        (126..129, "hex"),
        (169..172, "rgb"),
    ] {
        let parameter = identifier(&tree, span);
        assert_eq!(parameter.utf8_text(SKETCH_PALETTE).unwrap(), name);
        assert_eq!(parameter.parent().unwrap().kind(), "block_params");
    }
}

#[test]
fn original_openzeppelin_templates_parse_completely_without_block_parameters() {
    for (name, source, bytes) in [
        ("contract", OPENZEPPELIN_CONTRACT, 2870),
        ("page", OPENZEPPELIN_PAGE, 137),
    ] {
        assert_eq!(source.len(), bytes, "{name}");
        let tree = clean(source);
        let all = nodes(&tree);
        assert!(
            all.iter().any(|node| node.kind() == "block_statement"),
            "{name}"
        );
        assert!(
            !all.iter().any(|node| node.kind() == "block_params"),
            "{name}"
        );
    }
}

#[test]
fn triple_values_and_zero_argument_blocks_keep_existing_value_node_shapes() {
    for source in [
        "{{#noop}}{{{item.name}}}{{/noop}}",
        "{{~#noop~}}{{~{item.name}~}}{{~/noop~}}",
    ] {
        let tree = clean(source.as_bytes());
        let start = source.find("item.name").unwrap();
        let head = identifier(&tree, start..start + 4);
        let path = head.parent().unwrap();
        assert_eq!(path.kind(), "path_expression");
        assert_eq!(path.parent().unwrap().kind(), "mustache_statement");
        assert_eq!(enclosing_block(head).kind(), "block_statement");
        assert!(
            !nodes(&tree)
                .iter()
                .any(|node| node.kind() == "block_params")
        );
    }
    for (prefix, literal) in [("", false), (r"\", true), (r"\\", false), (r"\\\", false)] {
        let source = [prefix, "{{{item.name}}} {{{item.name}}}"].concat();
        let tree = clean(source.as_bytes());
        let first = source.find("item.name").unwrap();
        assert_eq!(
            nodes(&tree)
                .iter()
                .filter(
                    |node| node.kind() == "identifier" && node.byte_range() == (first..first + 4)
                )
                .count(),
            if literal { 0 } else { 1 },
            "{source}"
        );
        let genuine = source.rfind("item.name").unwrap();
        identifier(&tree, genuine..genuine + 4);
    }
}

#[test]
fn data_paths_keep_their_whole_token_and_reject_incomplete_or_split_forms() {
    for data in ["@last", "@index", "@../last", "@../../index"] {
        let source = ["{{", data, "}}"].concat();
        let tree = clean(source.as_bytes());
        let paths: Vec<_> = nodes(&tree)
            .into_iter()
            .filter(|node| node.kind() == "data_path_expression")
            .collect();
        assert_eq!(paths.len(), 1, "{source}");
        assert_eq!(paths[0].byte_range(), 2..2 + data.len(), "{source}");
        assert_eq!(paths[0].utf8_text(source.as_bytes()).unwrap(), data);
        assert_eq!(paths[0].parent().unwrap().kind(), "mustache_statement");
    }
    for data in ["@", "@../", "@.. /last", "@../ last", "@@last"] {
        let source = ["{{", data, "}}"].concat();
        let tree = parse(source.as_bytes());
        assert!(
            tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn xml_declaration_and_whitespace_strip_delimiters_remain_written_syntax() {
    let source = br#"<?xml version="1.0"?>
<!DOCTYPE palette>
<palette>{{~#each colors as |color|~}}<entry>{{color.name}}</entry>{{~/each~}}</palette>"#;
    let tree = clean(source);
    let all = nodes(&tree);
    for kind in [
        "processing_instruction",
        "doctype_declaration",
        "{{~#",
        "{{~/",
        "~}}",
    ] {
        assert!(all.iter().any(|node| node.kind() == kind), "missing {kind}");
    }
}

#[test]
fn truncated_template_and_markup_require_native_recovery() {
    for source in [
        b"{{#each colors as |color|}}{{color.name}}".as_slice(),
        b"<?xml version=\"1.0\"".as_slice(),
        b"{{#each values as |item|}}{{item.name}{{/each}}".as_slice(),
        b"{{{item.name}}".as_slice(),
        b"{{{}}}".as_slice(),
        b"{{{~item.name}}}".as_slice(),
        b"{{{item.name~}}}".as_slice(),
        b"{{#noop}}{{item.name}}".as_slice(),
        br#"<!DOCTYPE palette [<!ENTITY shade "blue">]><palette/>"#.as_slice(),
    ] {
        let tree = parse(source);
        assert!(
            tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn inline_value_strip_is_a_delimiter_outside_the_identifier() {
    let source = b"{{#each values as |item|}}{{item~}}{{/each}}";
    let tree = clean(source);
    let start = source
        .windows(b"{{item~}}".len())
        .position(|part| part == b"{{item~}}")
        .unwrap()
        + 2;
    let value = identifier(&tree, start..start + 4);
    assert_eq!(value.utf8_text(source).unwrap(), "item");
    assert_eq!(value.parent().unwrap().kind(), "mustache_statement");
    assert_eq!(value.next_sibling().unwrap().kind(), "~}}");
}

#[test]
fn inline_escapes_preserve_literal_heads_and_real_block_topology() {
    for (prefix, literal) in [
        ("", false),
        (r"\", true),
        (r"\\", false),
        (r"\\\", false),
        (r"\\\\", false),
        (r"\ ", false),
        (r"\\ ", false),
    ] {
        let source = [
            "{{#each values as |item|}}",
            prefix,
            "{{item.name}} {{item.name}}{{/each}}",
        ]
        .concat();
        let tree = clean(source.as_bytes());
        let all = nodes(&tree);
        let first = source.find("item.name").unwrap();
        assert_eq!(
            all.iter()
                .filter(
                    |node| node.kind() == "identifier" && node.byte_range() == (first..first + 4)
                )
                .count(),
            if literal { 0 } else { 1 },
            "{source}"
        );
        let genuine = source.rfind("item.name").unwrap();
        identifier(&tree, genuine..genuine + 4);
        assert_eq!(
            all.iter()
                .filter(|node| node.kind() == "block_statement")
                .count(),
            1,
            "{source}"
        );
        assert_eq!(
            all.iter()
                .filter(|node| node.kind() == "block_params")
                .count(),
            1,
            "{source}"
        );
    }
    for body in [
        r"\{{#with other as |ghost|}}\{{/with}}\{{/each}}",
        r"\{{~#with other as |ghost|~}}\{{~/with~}}\{{~/each~}}",
        r#"<span data-name="\{{item.name}}"></span>"#,
    ] {
        let source = [
            "{{#each values as |item|}}{{item.name}} ",
            body,
            " {{item.name}}{{/each}}",
        ]
        .concat();
        let tree = clean(source.as_bytes());
        let all = nodes(&tree);
        assert_eq!(
            all.iter()
                .filter(|node| node.kind() == "block_statement")
                .count(),
            1,
            "{source}"
        );
        assert_eq!(
            all.iter()
                .filter(|node| node.kind() == "block_params")
                .count(),
            1,
            "{source}"
        );
        assert!(
            !all.iter().any(|node| node.kind() == "identifier"
                && node.utf8_text(source.as_bytes()).unwrap() == "ghost"),
            "{source}"
        );
        if let Some(escaped) = source.find(r"\{{item.name}}") {
            let start = escaped + 3;
            assert!(
                !all.iter()
                    .any(|node| node.kind() == "identifier"
                        && node.byte_range() == (start..start + 4)),
                "{source}"
            );
        }
    }
}
