//! Compiles authenticated Rocq parser tables with declared local C tools.
//! Authored generator inputs and other package hooks remain inert source data.

fn main() {
    let mut builder = cc::Build::new();
    builder.std("c11").include("src").file("src/parser.c");
    #[cfg(target_env = "msvc")]
    builder.flag("-utf-8");
    builder.compile("tree-sitter-rocq");
    for source in ["src/parser.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={source}");
    }
}
