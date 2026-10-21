//! Compile retained C11 tables without acquisition or grammar generation.
fn main() {
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("upstream/src")
        .file("upstream/src/parser.c");
    if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|value| value == "msvc") {
        build.flag("/utf-8");
    }
    build.compile("tree-sitter-meson");
    for path in ["upstream/src/parser.c", "upstream/src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
