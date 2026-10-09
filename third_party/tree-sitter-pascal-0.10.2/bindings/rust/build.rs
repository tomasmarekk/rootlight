//! Compiles only the reviewed, locally generated Pascal static table.
//! Source acquisition, grammar generation, and upstream package hooks are
//! deliberately absent; the original build input is retained separately.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .flag_if_supported("-utf-8")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs")
        .compile("tree-sitter-pascal");

    for path in ["src/parser.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
