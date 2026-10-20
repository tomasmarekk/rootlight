fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c")
        .compile("tree-sitter-hcl");
    for input in ["src/parser.c", "src/scanner.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={input}");
    }
}
