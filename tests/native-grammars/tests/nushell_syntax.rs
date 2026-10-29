//! Native syntax witnesses for the retained Nushell grammar.
//! These tests parse exact source bytes and never execute shell commands.

use tree_sitter::{Parser, Tree};

const ORIGINAL: &[u8] =
    include_bytes!("../../../crates/rootlight-adapter-treesitter/tests/fixtures/nushell/after.nu");

fn parse(source: &[u8]) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_nu::LANGUAGE.into())
        .expect("reviewed ABI is supported");
    parser.parse(source, None).expect("native parse completes")
}

#[test]
fn original_after_command_has_complete_native_function_and_parameter_nodes() {
    assert_eq!(ORIGINAL.len(), 452);
    let tree = parse(ORIGINAL);
    println!("{}", tree.root_node().to_sexp());
    assert!(!tree.root_node().has_error());
    assert_eq!(tree.root_node().kind(), "nu_script");
    let mut cursor = tree.root_node().walk();
    let functions = tree
        .root_node()
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "decl_def")
        .count();
    assert_eq!(functions, 3);
}

#[test]
fn truncated_headers_require_native_recovery() {
    for source in [
        b"def missing [pid: string".as_slice(),
        b"def incomplete [] { print $value".as_slice(),
    ] {
        assert!(parse(source).root_node().has_error());
    }
}

#[test]
fn product_query_compiles_against_the_retained_native_nodes() {
    tree_sitter::Query::new(
        &tree_sitter_nu::LANGUAGE.into(),
        include_str!("../../../crates/rootlight-adapter-treesitter/queries/nu.scm"),
    )
    .expect("reviewed product query matches the native grammar");
}
