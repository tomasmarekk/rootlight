//! Compile the retained GraphQL ABI14 parser with its original paired header.
//! Acquisition, grammar generation and optional upstream Node bindings are separate.

fn main() {
    let mut build = cc::Build::new();
    build.std("c11").include("src").file("src/parser.c");
    #[cfg(target_env = "msvc")]
    build.flag("-utf-8");
    build.compile("tree-sitter-graphql");
    for path in ["src/parser.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
