//! Compile only the reviewed local parser and scanner with the declared C toolchain.
fn main() {
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c");
    if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|environment| environment == "msvc") {
        build.flag("/utf-8");
    }
    build.compile("tree-sitter-ring");
    for path in [
        "src/parser.c",
        "src/scanner.c",
        "src/tree_sitter/parser.h",
        "src/tree_sitter/alloc.h",
        "src/tree_sitter/array.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}
