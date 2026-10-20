//! Compile complete local native inputs without generators or acquisition.
fn main() {
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("upstream/src")
        .file("upstream/src/parser.c")
        .file("src/scanner.c");
    if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|value| value == "msvc") {
        build.flag("/utf-8");
    }
    build.compile("tree-sitter-sml");
    for path in [
        "upstream/src/parser.c",
        "upstream/src/scanner.c",
        "src/scanner.c",
        "upstream/src/tree_sitter/alloc.h",
        "upstream/src/tree_sitter/array.h",
        "upstream/src/tree_sitter/parser.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}
