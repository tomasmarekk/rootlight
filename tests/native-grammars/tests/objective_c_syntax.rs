//! Objective-C syntax qualification with exact source and selector boundaries.
//! Native parsing alone does not establish type resolution or runtime dispatch.

use tree_sitter::{InputEdit, Node, Parser, Point};

const SOURCE: &str = include_str!("../../fixtures/objective-c/declarations.m");

fn parser() -> Parser {
    let mut parser = Parser::new();
    let language = tree_sitter_objc::LANGUAGE.into();
    parser.set_language(&language).unwrap();
    parser
}

fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut output = Vec::new();
    while let Some(node) = pending.pop() {
        output.push(node);
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    output.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind_id()));
    output
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
fn declarations_implementations_categories_and_messages_are_distinct() {
    let language: tree_sitter::Language = tree_sitter_objc::LANGUAGE.into();
    assert_eq!(language.abi_version(), 14);
    let tree = parser().parse(SOURCE, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().byte_range(), 0..SOURCE.len());
    let all = nodes(tree.root_node());
    for (kind, count) in [
        ("class_interface", 3),
        ("class_implementation", 3),
        ("protocol_declaration", 1),
        ("method_declaration", 5),
        ("method_definition", 4),
        ("property_declaration", 1),
        ("message_expression", 4),
        ("function_definition", 1),
    ] {
        assert_eq!(
            all.iter().filter(|node| node.kind() == kind).count(),
            count,
            "{kind}"
        );
    }
    for node in &all {
        assert!(SOURCE.get(node.byte_range()).is_some());
    }
    let categories: Vec<_> = all
        .iter()
        .filter_map(|node| node.child_by_field_name("category"))
        .map(|node| node.utf8_text(SOURCE.as_bytes()).unwrap())
        .collect();
    assert_eq!(categories, ["Scaling", "Scaling"]);
    let inherited: Vec<_> = all
        .iter()
        .filter_map(|node| node.child_by_field_name("superclass"))
        .map(|node| node.utf8_text(SOURCE.as_bytes()).unwrap())
        .collect();
    assert_eq!(inherited, ["Counter"]);
}

#[test]
fn multi_part_message_fields_exclude_receiver_and_argument_identifiers() {
    let source =
        "int run(id receiver, int left, int right) { return [receiver add:left to:right]; }";
    let tree = parser().parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    let all = nodes(tree.root_node());
    let message = all
        .iter()
        .find(|node| node.kind() == "message_expression")
        .unwrap();
    let mut cursor = message.walk();
    let method: Vec<_> = message
        .children_by_field_name("method", &mut cursor)
        .map(|node| node.utf8_text(source.as_bytes()).unwrap())
        .collect();
    assert_eq!(method, ["add", "to"]);
    assert_eq!(
        message
            .child_by_field_name("receiver")
            .unwrap()
            .utf8_text(source.as_bytes())
            .unwrap(),
        "receiver"
    );
}

#[test]
fn empty_selector_components_remain_valid_method_and_message_syntax() {
    let source = include_str!("../../fixtures/objective-c/selectors.m");
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let all = nodes(tree.root_node());
    assert_eq!(
        all.iter()
            .filter(|node| node.kind() == "method_declaration")
            .count(),
        4
    );
    assert_eq!(
        all.iter()
            .filter(|node| node.kind() == "message_expression")
            .count(),
        4
    );
}

#[test]
fn message_selectors_require_colons_and_nonempty_argument_expressions() {
    for expression in [
        "[receiver]",
        "[receiver first second]",
        "[receiver first:]",
        "[receiver :]",
    ] {
        let source = format!("int run(id receiver) {{ return {expression}; }}");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{expression}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn comment_and_string_contents_do_not_declare_objective_c_entities() {
    let source = "// @interface Phantom @end\nconst char *text = \"@implementation Hidden @end\";\nint real(void) { return 1; }\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    let all = nodes(tree.root_node());
    assert!(
        !all.iter()
            .any(|node| matches!(node.kind(), "class_interface" | "class_implementation"))
    );
    assert_eq!(
        all.iter()
            .filter(|node| node.kind() == "function_definition")
            .count(),
        1
    );
}

#[test]
fn body_edit_matches_fresh_parse_without_losing_source_ranges() {
    let mut parser = parser();
    let mut old = parser.parse(SOURCE, None).unwrap();
    let start = SOURCE.find("return 1;").unwrap() + "return ".len();
    let mut changed = SOURCE.to_owned();
    changed.replace_range(start..start + 1, "20");
    old.edit(&InputEdit {
        start_byte: start,
        old_end_byte: start + 1,
        new_end_byte: start + 2,
        start_position: point(SOURCE, start),
        old_end_position: point(SOURCE, start + 1),
        new_end_position: point(&changed, start + 2),
    });
    let incremental = parser.parse(&changed, Some(&old)).unwrap();
    let fresh = parser.parse(&changed, None).unwrap();
    assert!(!incremental.root_node().has_error());
    let signature = |tree: &tree_sitter::Tree| {
        nodes(tree.root_node())
            .into_iter()
            .map(|node| (node.kind().to_owned(), node.byte_range(), node.is_named()))
            .collect::<Vec<_>>()
    };
    assert_eq!(signature(&incremental), signature(&fresh));
}

#[test]
fn malformed_methods_retain_error_evidence_with_in_bounds_ranges() {
    let source = "@implementation Broken\n- (void)send:( { [self unfinished: ;\n@end";
    let tree = parser().parse(source, None).unwrap();
    assert!(tree.root_node().has_error());
    assert!(
        nodes(tree.root_node())
            .into_iter()
            .all(|node| source.get(node.byte_range()).is_some())
    );
}
