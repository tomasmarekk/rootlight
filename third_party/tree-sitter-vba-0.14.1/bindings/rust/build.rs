//! Compiles checked-in native grammar tables without build-time generation.
//! The VBA dialect has no external scanner or runtime source dependencies.

fn main() {
    let mut build = cc::Build::new();
    build.std("c11").include("vba/src");
    build.flag_if_supported("-utf-8");
    build.file("vba/src/parser.c").compile("tree-sitter-vba");
    println!("cargo:rerun-if-changed=vba/src/parser.c");
    println!("cargo:rerun-if-changed=vba/src/tree_sitter/parser.h");
}
