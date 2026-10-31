//! Compile only the retained upstream SPARQL parser and headers.
//! Acquisition, generation and upstream lifecycle hooks never run during builds.

fn main() {
    let mut build = cc::Build::new();
    build.std("c11").include("src").file("src/parser.c");
    #[cfg(target_env = "msvc")]
    build.flag("-utf-8");
    build.compile("tree-sitter-sparql");
    for path in [
        "src/parser.c",
        "src/tree_sitter/parser.h",
        "src/tree_sitter/alloc.h",
        "src/tree_sitter/array.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}
