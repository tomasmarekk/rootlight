//! Compile only the retained local CUE parser, scanner and headers.
//! Upstream generation, package scripts and source acquisition never run here.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c")
        .compile("tree-sitter-cue");
    for source in [
        "src/parser.c",
        "src/scanner.c",
        "src/tree_sitter/parser.h",
        "src/tree_sitter/alloc.h",
    ] {
        println!("cargo:rerun-if-changed={source}");
    }
}
