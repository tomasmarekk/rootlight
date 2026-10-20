//! Conserve the reviewed Haxe native CST controls and original written fields.
//! Native BOM projection here is separate from product encoding qualification.

use tree_sitter::{InputEdit, Parser, Point};

const CASES: [(&str, &[u8], bool); 41] = [
    (
        "original.hx",
        include_bytes!("fixtures/haxe/original.hx"),
        false,
    ),
    (
        "ordinary.hx",
        include_bytes!("fixtures/haxe/ordinary.hx"),
        false,
    ),
    (
        "macro-modifier.hx",
        include_bytes!("fixtures/haxe/macro-modifier.hx"),
        false,
    ),
    (
        "block-reification.hx",
        include_bytes!("fixtures/haxe/block-reification.hx"),
        false,
    ),
    (
        "value-reification.hx",
        include_bytes!("fixtures/haxe/value-reification.hx"),
        false,
    ),
    (
        "literal-lookalike.hx",
        include_bytes!("fixtures/haxe/literal-lookalike.hx"),
        false,
    ),
    (
        "conditional.hx",
        include_bytes!("fixtures/haxe/conditional.hx"),
        false,
    ),
    (
        "block-shadow.hx",
        include_bytes!("fixtures/haxe/block-shadow.hx"),
        false,
    ),
    (
        "original-utf8-bom.hx",
        include_bytes!("fixtures/haxe/original-utf8-bom.hx"),
        false,
    ),
    (
        "macro-ordinary-expression.hx",
        include_bytes!("fixtures/haxe/macro-ordinary-expression.hx"),
        false,
    ),
    (
        "macro-empty-block-no-semicolon.hx",
        include_bytes!("fixtures/haxe/macro-empty-block-no-semicolon.hx"),
        false,
    ),
    (
        "bare-try-catch.hx",
        include_bytes!("fixtures/haxe/bare-try-catch.hx"),
        false,
    ),
    (
        "block-try-catch.hx",
        include_bytes!("fixtures/haxe/block-try-catch.hx"),
        false,
    ),
    (
        "wildcard-catch.hx",
        include_bytes!("fixtures/haxe/wildcard-catch.hx"),
        false,
    ),
    (
        "multiple-catches.hx",
        include_bytes!("fixtures/haxe/multiple-catches.hx"),
        false,
    ),
    (
        "unknown-reification-selector.hx",
        include_bytes!("fixtures/haxe/unknown-reification-selector.hx"),
        true,
    ),
    (
        "unclosed-reification.hx",
        include_bytes!("fixtures/haxe/unclosed-reification.hx"),
        true,
    ),
    (
        "missing-macro-operand.hx",
        include_bytes!("fixtures/haxe/missing-macro-operand.hx"),
        true,
    ),
    (
        "try-missing-catch.hx",
        include_bytes!("fixtures/haxe/try-missing-catch.hx"),
        true,
    ),
    (
        "catch-missing-delimiter.hx",
        include_bytes!("fixtures/haxe/catch-missing-delimiter.hx"),
        true,
    ),
    (
        "reification-outside-macro.hx",
        include_bytes!("fixtures/haxe/reification-outside-macro.hx"),
        true,
    ),
    (
        "nested-block-splice-queued.hx",
        include_bytes!("fixtures/haxe/nested-block-splice-queued.hx"),
        true,
    ),
    (
        "type-reification-queued.hx",
        include_bytes!("fixtures/haxe/type-reification-queued.hx"),
        true,
    ),
    (
        "literal-and-comment-boundaries.hx",
        include_bytes!("fixtures/haxe/literal-and-comment-boundaries.hx"),
        false,
    ),
    (
        "selector-empty.hx",
        include_bytes!("fixtures/haxe/selector-empty.hx"),
        false,
    ),
    (
        "selector-e.hx",
        include_bytes!("fixtures/haxe/selector-e.hx"),
        false,
    ),
    (
        "selector-v.hx",
        include_bytes!("fixtures/haxe/selector-v.hx"),
        false,
    ),
    (
        "selector-i.hx",
        include_bytes!("fixtures/haxe/selector-i.hx"),
        false,
    ),
    (
        "selector-a.hx",
        include_bytes!("fixtures/haxe/selector-a.hx"),
        false,
    ),
    (
        "selector-b.hx",
        include_bytes!("fixtures/haxe/selector-b.hx"),
        false,
    ),
    (
        "selector-p.hx",
        include_bytes!("fixtures/haxe/selector-p.hx"),
        false,
    ),
    (
        "try-conditional-conservation.hx",
        include_bytes!("fixtures/haxe/try-conditional-conservation.hx"),
        false,
    ),
    (
        "try-loop-conservation.hx",
        include_bytes!("fixtures/haxe/try-loop-conservation.hx"),
        false,
    ),
    (
        "macro-complete-body-boundary.hx",
        include_bytes!("fixtures/haxe/macro-complete-body-boundary.hx"),
        false,
    ),
    (
        "preprocessor-macro-modifier.hx",
        include_bytes!("fixtures/haxe/preprocessor-macro-modifier.hx"),
        false,
    ),
    (
        "preprocessor-boolean-flags.hx",
        include_bytes!("fixtures/haxe/preprocessor-boolean-flags.hx"),
        false,
    ),
    (
        "preprocessor-constant-comparisons.hx",
        include_bytes!("fixtures/haxe/preprocessor-constant-comparisons.hx"),
        false,
    ),
    (
        "preprocessor-elseif-owners.hx",
        include_bytes!("fixtures/haxe/preprocessor-elseif-owners.hx"),
        false,
    ),
    (
        "preprocessor-missing-rhs.hx",
        include_bytes!("fixtures/haxe/preprocessor-missing-rhs.hx"),
        true,
    ),
    (
        "preprocessor-unclosed-group.hx",
        include_bytes!("fixtures/haxe/preprocessor-unclosed-group.hx"),
        true,
    ),
    (
        "preprocessor-runtime-call-rejected.hx",
        include_bytes!("fixtures/haxe/preprocessor-runtime-call-rejected.hx"),
        true,
    ),
];

#[test]
fn reviewed_haxe_syntax_cases_and_newline_edits_keep_their_native_shapes() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_haxe::LANGUAGE.into())
        .unwrap();
    for (name, raw, recovered) in CASES {
        let source = raw.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(raw);
        let mut tree = parser.parse(source, None).unwrap();
        assert_eq!(
            tree.root_node().has_error(),
            recovered,
            "{name}: {}",
            tree.root_node().to_sexp()
        );
        let mut updated = source.to_vec();
        updated.push(b'\n');
        let row = source.iter().filter(|byte| **byte == b'\n').count();
        let column = source
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(source.len(), |offset| source.len() - offset - 1);
        tree.edit(&InputEdit {
            start_byte: source.len(),
            old_end_byte: source.len(),
            new_end_byte: updated.len(),
            start_position: Point::new(row, column),
            old_end_position: Point::new(row, column),
            new_end_position: Point::new(row + 1, 0),
        });
        let reused = parser.parse(&updated, Some(&tree)).unwrap();
        let fresh = parser.parse(&updated, None).unwrap();
        assert_eq!(
            reused.root_node().to_sexp(),
            fresh.root_node().to_sexp(),
            "{name}"
        );
        assert_eq!(
            reused.root_node().byte_range(),
            fresh.root_node().byte_range(),
            "{name}"
        );
    }
}

#[test]
fn original_haxe_name_fields_are_exact_and_macro_host_is_not_a_quotation() {
    let source = include_bytes!("fixtures/haxe/original.hx");
    assert_eq!(source.len(), 1843);
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_haxe::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    for (start, end, parent_kind, field) in [
        (6, 16, "class_declaration", "name"),
        (464, 487, "function_declaration", "name"),
        (526, 529, "variable_declaration", "name"),
        (609, 612, "call_expression", "arguments_list"),
    ] {
        let node = tree
            .root_node()
            .named_descendant_for_byte_range(start, end)
            .unwrap();
        assert_eq!(node.kind(), "identifier");
        assert_eq!(node.byte_range(), start..end);
        let parent = node.parent().unwrap();
        assert_eq!(parent.kind(), parent_kind);
        let mut cursor = parent.walk();
        let index = parent
            .children(&mut cursor)
            .position(|child| child == node)
            .unwrap();
        assert_eq!(
            parent.field_name_for_child(u32::try_from(index).unwrap()),
            Some(field)
        );
    }
}
