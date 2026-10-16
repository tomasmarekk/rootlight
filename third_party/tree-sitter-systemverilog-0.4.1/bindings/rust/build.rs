//! Compiles authenticated HDL parser inputs with declared local tools.
//! Upstream acquisition, generation and package lifecycle hooks stay inert.

fn main() {
    let mut builder = cc::Build::new();
    builder.std("c11").include("src").file("src/parser.c");
    #[cfg(target_env = "msvc")]
    builder.flag("-utf-8");
    builder.compile("tree-sitter-systemverilog");
    for source in ["src/parser.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={source}");
    }
}
