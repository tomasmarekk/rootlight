//! Native Zig checks preserve written binders and explicit recovery limitations.
//! Clean CSTs establish syntax, never comptime evaluation or runtime target choice.

use tree_sitter::{InputEdit, Parser, Point};

const SOURCE: &str = include_str!("../../tests/fixtures/pipe.zig");

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser.set_language(&super::LANGUAGE.into()).unwrap();
    parser
}

#[test]
fn original_function_and_branch_local_binders_keep_exact_native_leaves() {
    assert_eq!(SOURCE.len(), 772);
    let mut parser = parser();
    let tree = parser.parse(SOURCE, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    for (start, end, name, parent) in [
        (298, 302, "pipe", "function_declaration"),
        (452, 456, "read", "variable_declaration"),
        (502, 507, "write", "variable_declaration"),
    ] {
        let leaf = tree
            .root_node()
            .named_descendant_for_byte_range(start, end)
            .unwrap();
        assert_eq!(leaf.kind(), "identifier");
        assert_eq!(leaf.byte_range(), start..end);
        assert_eq!(leaf.utf8_text(SOURCE.as_bytes()).unwrap(), name);
        assert_eq!(leaf.parent().unwrap().kind(), parent);
    }
    let address_use = tree
        .root_node()
        .named_descendant_for_byte_range(586, 590)
        .unwrap();
    assert_eq!(address_use.kind(), "identifier");
    assert_eq!(address_use.parent().unwrap().kind(), "unary_expression");
    assert_ne!(address_use.parent().unwrap().kind(), "variable_declaration");
}

#[test]
fn legal_empty_container_syntax_and_real_edits_preserve_incremental_trees() {
    let source = concat!(
        "const S = struct {};\nconst O = opaque {};\n",
        "const E = enum {};\nconst U = union {};\n",
        "fn written() void { var local: u8 = 1; _ = &local; }\n"
    );
    let mut parser = parser();
    let mut original = parser.parse(source, None).unwrap();
    assert!(!original.root_node().has_error());
    let suffix = "const after = 1;\n";
    let updated = format!("{source}{suffix}");
    let end = original.root_node().end_position();
    original.edit(&InputEdit {
        start_byte: source.len(),
        old_end_byte: source.len(),
        new_end_byte: updated.len(),
        start_position: end,
        old_end_position: end,
        new_end_position: Point::new(end.row + 1, 0),
    });
    let incremental = parser.parse(&updated, Some(&original)).unwrap();
    let fresh = parser.parse(&updated, None).unwrap();
    assert!(!fresh.root_node().has_error());
    assert_eq!(
        incremental.root_node().to_sexp(),
        fresh.root_node().to_sexp()
    );
    assert_eq!(incremental.root_node().end_byte(), updated.len());
    for invalid in [
        "const S = struct {;\n",
        "const O = opaque {;\n",
        "const E = enum {;\n",
        "const U = union {;\n",
        "fn missing( {\n",
    ] {
        assert!(
            parser.parse(invalid, None).unwrap().root_node().has_error(),
            "{invalid}"
        );
    }
}

#[test]
fn six_original_recovery_files_remain_explicit_applicable_parser_gaps() {
    let mut parser = parser();
    for (path, raw, bytes) in [
        (
            "pkg/highway/src/detect/x86.zig",
            include_str!("../../tests/fixtures/recovery/pkg/highway/src/detect/x86.zig"),
            5446,
        ),
        (
            "src/Command.zig",
            include_str!("../../tests/fixtures/recovery/src/Command.zig"),
            37939,
        ),
        (
            "src/Surface.zig",
            include_str!("../../tests/fixtures/recovery/src/Surface.zig"),
            223407,
        ),
        (
            "src/global.zig",
            include_str!("../../tests/fixtures/recovery/src/global.zig"),
            16118,
        ),
        (
            "src/main_c.zig",
            include_str!("../../tests/fixtures/recovery/src/main_c.zig"),
            9717,
        ),
        (
            "src/quirks_memset.zig",
            include_str!("../../tests/fixtures/recovery/src/quirks_memset.zig"),
            13476,
        ),
    ] {
        assert_eq!(raw.len(), bytes, "{path}");
        let tree = parser.parse(raw, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "Known syntax gap changed: diagnose and update its explicit support status: {path}"
        );
    }
}
