//! Compiles retained CMake parser/scanner inputs with the declared local tools.
//! Archived upstream build, acquisition and generation hooks are never invoked.

fn main() {
    let mut builder = cc::Build::new();
    builder
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c");
    #[cfg(target_env = "msvc")]
    builder.flag("-utf-8");
    builder.compile("tree-sitter-cmake");
    for source in [
        "src/parser.c",
        "src/scanner.c",
        "src/tree_sitter/parser.h",
        "src/tree_sitter/alloc.h",
        "src/tree_sitter/array.h",
    ] {
        println!("cargo:rerun-if-changed={source}");
    }
}
