//! Compiles retained protobuf parser inputs without acquisition or generation.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .compile("tree-sitter-proto");
    for source in ["src/parser.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={source}");
    }
}
