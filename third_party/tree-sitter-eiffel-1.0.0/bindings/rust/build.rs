//! Compiles retained Eiffel parser and scanner inputs without acquisition or generation.

fn main() {
    let mut builder = cc::Build::new();
    builder
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c");
    #[cfg(target_env = "msvc")]
    builder.flag("-utf-8");
    builder.compile("tree-sitter-eiffel");
    for source in [
        "src/parser.c",
        "src/scanner.c",
        "src/operator_symbols.h",
        "src/tree_sitter/parser.h",
    ] {
        println!("cargo:rerun-if-changed={source}");
    }
}
