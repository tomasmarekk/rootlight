//! Compile only the retained TypeSpec parser with the declared native toolchain.
//! Acquisition, grammar generation and upstream lifecycle hooks are not build steps.

fn main() {
    let mut build = cc::Build::new();
    build.std("c11").include("src").file("src/parser.c");
    #[cfg(target_env = "msvc")]
    build.flag("-utf-8");
    build.compile("tree-sitter-typespec");
    for path in ["src/parser.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
