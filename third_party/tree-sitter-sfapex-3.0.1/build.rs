//! Compiles only the authenticated, locally supplied Apex static table.
//! The enclosing vendored source inventory supplies parser bytes and headers;
//! no grammar generation, package lifecycle, or acquisition occurs here.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("apex/src")
        .file("apex/src/parser.c")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs")
        .compile("tree-sitter-apex");

    for path in [
        "apex/src/parser.c",
        "apex/src/tree_sitter/parser.h",
        "apex/src/tree_sitter/alloc.h",
        "apex/src/tree_sitter/array.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}
