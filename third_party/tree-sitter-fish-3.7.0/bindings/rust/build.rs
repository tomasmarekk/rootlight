//! Compile retained Fish C tables and scanner without generation or acquisition.
//! The original multi-language package scripts are not native build inputs.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c")
        .compile("tree-sitter-fish");
    for source in ["src/parser.c", "src/scanner.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={source}");
    }
}
