//! Compile retained Pony C tables and scanner without generation or acquisition.
//! Upstream package scripts and example checkouts are not native build inputs.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c")
        .compile("tree-sitter-pony");
    for source in ["src/parser.c", "src/scanner.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={source}");
    }
}
