//! Compiles the retained Clojure ABI13 parser and its original paired header.
//! No upstream lifecycle script, parser generation or acquisition runs here.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .compile("tree-sitter-clojure");
    println!("cargo:rerun-if-changed=src/parser.c");
    println!("cargo:rerun-if-changed=src/tree_sitter/parser.h");
}
