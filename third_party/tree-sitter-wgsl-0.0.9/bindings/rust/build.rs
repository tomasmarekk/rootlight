//! Compiles only the retained ABI14 WGSL table and hardened external scanner.
//! Acquisition, generation and archived upstream lifecycle hooks never run here.

fn main() {
    let mut builder = cc::Build::new();
    builder
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c");
    #[cfg(target_env = "msvc")]
    builder.flag("-utf-8");
    builder.compile("tree-sitter-wgsl");

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
