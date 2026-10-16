//! Compiles authenticated parser tables with declared local C tools.
//! Source acquisition, upstream generation and package lifecycle hooks stay inert.

fn main() {
    let mut builder = cc::Build::new();
    builder.std("c11").include("src").file("src/parser.c");
    #[cfg(target_env = "msvc")]
    builder.flag("-utf-8");
    builder.compile("tree-sitter-v");
    for source in ["src/parser.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={source}");
    }
}
