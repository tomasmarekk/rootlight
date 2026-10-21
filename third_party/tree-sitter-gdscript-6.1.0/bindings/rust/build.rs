//! Compile only retained parser tables and the request-scoped scanner derivative.
//! Acquisition and scanner qualification are separate from this offline build.

fn main() {
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("upstream/src")
        .include("active-src")
        .file("upstream/src/parser.c")
        .file("active-src/scanner.c")
        .file("active-src/rootlight_gdscript_status.c");
    if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|environment| environment == "msvc") {
        build.flag("/utf-8");
    }
    build.compile("tree-sitter-gdscript");
    for path in [
        "upstream/src/parser.c",
        "upstream/src/tree_sitter/parser.h",
        "upstream/src/tree_sitter/alloc.h",
        "upstream/src/tree_sitter/array.h",
        "active-src/scanner.c",
        "active-src/rootlight_gdscript_status.c",
        "active-src/rootlight_gdscript_status.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}
