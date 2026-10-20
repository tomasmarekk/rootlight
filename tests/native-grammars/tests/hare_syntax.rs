//! Native syntax is qualified separately from Rootlight declarations and binding.

fn parse(source: &[u8]) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_hare::LANGUAGE.into();
    parser.set_language(&language).unwrap();
    parser.parse(source, None).unwrap()
}

#[test]
fn original_hare_tree_retains_each_required_clean_leaf() {
    let source = include_bytes!(
        "../../../crates/rootlight-adapter-treesitter/tests/fixtures/hare/original-part1.ha"
    );
    assert_eq!(source.len(), 1742);
    let language: tree_sitter::Language = tree_sitter_hare::LANGUAGE.into();
    assert_eq!(language.abi_version(), 15);
    let tree = parse(source);
    assert!(!tree.root_node().has_error());
    for (start, end, written) in [
        (63, 71, "equation"),
        (85, 89, "test"),
        (121, 131, "read_input"),
        (1408, 1412, "main"),
        (1444, 1454, "read_input"),
    ] {
        let node = tree
            .root_node()
            .named_descendant_for_byte_range(start, end)
            .unwrap();
        assert_eq!(node.kind(), "identifier");
        assert_eq!(node.byte_range(), start..end);
        assert!(!node.has_error() && !node.is_missing());
        assert_eq!(&source[start..end], written.as_bytes());
    }
}

#[test]
fn native_foreach_modes_and_accumulator_keep_distinct_binding_fields() {
    for source in [
        "fn run() void = { for (const value .. values) { use_value(value); }; };",
        "fn run() void = { for (let value: int .. values) { use_value(value); }; };",
        "fn run() void = { for (let value &.. values) { use_value(value); }; };",
        "fn run() void = { for (let value => values) { use_value(value); }; };",
        "fn run() void = { for (let i = 0; i < 3; i += 1) { use_value(i); }; };",
    ] {
        let tree = parse(source.as_bytes());
        assert!(!tree.root_node().has_error(), "{source}");
        let mut pending = vec![tree.root_node()];
        let mut bindings = Vec::new();
        while let Some(node) = pending.pop() {
            if node.kind() == "iterable_binding" {
                bindings.push(node);
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        if source.contains("i < 3") {
            assert!(bindings.is_empty());
        } else {
            assert_eq!(bindings.len(), 1);
            let binding = bindings[0].child_by_field_name("binding").unwrap();
            let iterable = bindings[0].child_by_field_name("iterable").unwrap();
            assert_eq!(&source[binding.byte_range()], "value");
            assert_eq!(&source[iterable.byte_range()], "values");
            assert_eq!(binding.kind(), "identifier");
            assert_eq!(iterable.kind(), "identifier");
        }
    }
    for source in [
        "fn run() void = { for (const value ..) {}; };",
        "fn run() void = { for (const .. values) {}; };",
        "fn run() void = { for (let (first, second) .. values) {}; };",
    ] {
        assert!(parse(source.as_bytes()).root_node().has_error(), "{source}");
    }
}
