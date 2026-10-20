//! Compile reviewed generated C and the unchanged scanner without acquisition.
fn main() {
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("upstream/src")
        .file("src/parser.c")
        .file("upstream/src/scanner.c");
    if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|v| v == "msvc") {
        build.flag("/utf-8");
    }
    build.compile("tree-sitter-qsharp");
    for path in [
        "src/parser.c",
        "src/tree_sitter/parser.h",
        "upstream/src/scanner.c",
        "upstream/src/tree_sitter/parser.h",
        "upstream/src/tree_sitter/alloc.h",
        "upstream/src/tree_sitter/array.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}
