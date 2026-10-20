fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .flag_if_supported("-utf-8")
        .file("src/parser.c")
        .file("src/scanner.c")
        .compile("tree-sitter-d");
    for input in ["src/parser.c", "src/scanner.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={input}");
    }
}
