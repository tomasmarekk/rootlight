//! Compile only the retained RON parser and external scanner.
//! Upstream acquisition, generation and lifecycle hooks are not build steps.

fn main() {
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c");
    #[cfg(target_env = "msvc")]
    build.flag("-utf-8");
    build.compile("tree-sitter-ron");
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
