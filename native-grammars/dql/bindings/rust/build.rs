//! Compile the retained native DQL parser and stateless lexical scanner.
//! Maintainer generation and source acquisition remain outside every build.

fn main() {
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c");
    #[cfg(target_env = "msvc")]
    build.flag("-utf-8");
    build.compile("rootlight-tree-sitter-dql");
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
