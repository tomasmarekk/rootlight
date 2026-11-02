//! Compiles retained VB.NET syntax tables without acquisition or generation.
//! Upstream Python hooks and prebuilt objects are never build inputs.

fn main() {
    let mut builder = cc::Build::new();
    builder.std("c11").include("src").file("src/parser.c");
    #[cfg(target_env = "msvc")]
    builder.flag("-utf-8");
    builder.compile("tree-sitter-vbnet");
    for source in ["src/parser.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={source}");
    }
}
