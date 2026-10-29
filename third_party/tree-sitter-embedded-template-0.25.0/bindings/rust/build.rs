//! Compile the retained native grammar without acquisition or generation.

fn main() {
    let mut builder = cc::Build::new();
    builder.std("c11").include("src").file("src/parser.c");
    #[cfg(target_env = "msvc")]
    builder.flag("-utf-8");
    builder.compile("tree-sitter-embedded-template");
    for path in ["src/parser.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
