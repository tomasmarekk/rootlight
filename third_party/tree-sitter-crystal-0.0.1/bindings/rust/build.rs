//! Compile the retained local Crystal tables, scanner and transitive headers.
//! Upstream lifecycle scripts and generation never run during the build.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c")
        .compile("tree-sitter-crystal");
    for source in [
        "src/parser.c",
        "src/scanner.c",
        "src/unicode.c",
        "src/tree_sitter/parser.h",
        "src/tree_sitter/array.h",
        "src/tree_sitter/alloc.h",
    ] {
        println!("cargo:rerun-if-changed={source}");
    }
}
