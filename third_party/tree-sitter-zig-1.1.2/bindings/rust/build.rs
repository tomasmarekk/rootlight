//! Compile retained local Zig tables; no source resolver or generator runs.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .compile("tree-sitter-zig");
    for source in [
        "src/parser.c",
        "src/tree_sitter/parser.h",
        "src/tree_sitter/array.h",
        "src/tree_sitter/alloc.h",
    ] {
        println!("cargo:rerun-if-changed={source}");
    }
}
