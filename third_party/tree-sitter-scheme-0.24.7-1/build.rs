//! Compile only the authenticated default-root parser, without generation.
//! Its original optimization-disabling pragmas are intentionally preserved.

fn main() {
    let parser = "upstream/src/parser.c";
    let header = "upstream/src/tree_sitter/parser.h";
    println!("cargo:rerun-if-changed={parser}");
    println!("cargo:rerun-if-changed={header}");
    cc::Build::new()
        .std("c11")
        .include("upstream/src")
        .file(parser)
        .compile("tree-sitter-scheme-native-local");
}
