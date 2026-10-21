//! Compile only previously generated, reviewed local C inputs.
//! Source generation is a separate preparation step, never a build action.

fn main() {
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c");
    if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|value| value == "msvc") {
        build.flag("/utf-8");
    }
    build.compile("tree-sitter-chapel");
    for path in ["src/parser.c", "src/scanner.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
