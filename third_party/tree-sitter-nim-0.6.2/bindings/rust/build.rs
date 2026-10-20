//! Compiles retained Nim tables and the paired bounded-state scanner locally.
//! Neither parser generation nor upstream package lifecycle scripts run here.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c")
        .compile("tree-sitter-nim");
    for path in [
        "src/parser.c",
        "src/scanner.c",
        "src/tree_sitter/parser.h",
        "src/tree_sitter/alloc.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}
