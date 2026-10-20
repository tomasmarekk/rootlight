//! Compiles the retained parser and reviewed scanner from local C11 inputs.
//! Upstream query-export checks and source generation are deliberately absent.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c")
        .compile("tree-sitter-vhdl");
    for source in SOURCES {
        println!("cargo:rerun-if-changed={source}");
    }
}

const SOURCES: &[&str] = &[
    "src/TokenTree.h",
    "src/TokenTree.inc",
    "src/TokenType.h",
    "src/TokenType.inc",
    "src/debug_macros.h",
    "src/parser.c",
    "src/scanner.c",
    "src/token_tree_match.inc",
    "src/tree_sitter/alloc.h",
    "src/tree_sitter/parser.h",
];
