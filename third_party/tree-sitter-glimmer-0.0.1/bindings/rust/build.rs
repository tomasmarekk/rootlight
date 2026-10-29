//! Compiles the retained native template table and its stateless scanner.
//! Archived upstream lifecycle hooks and generators never run during builds.

fn main() {
    let mut builder = cc::Build::new();
    builder
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c");
    #[cfg(target_env = "msvc")]
    builder.flag("-utf-8");
    builder.compile("tree-sitter-glimmer");
    for path in ["src/parser.c", "src/scanner.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
