//! Compile the retained native N-Quads parser and stateless lexical scanner.
//! The maintainer generator is deliberately outside compilation and packaging.

fn main() {
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c");
    #[cfg(target_env = "msvc")]
    build.flag("-utf-8");
    build.compile("rootlight-tree-sitter-nquads");
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
