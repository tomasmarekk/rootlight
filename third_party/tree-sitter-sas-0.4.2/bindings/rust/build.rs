//! Compile only the retained local SAS parser and scanner as a static library.
//! No source acquisition, grammar generator or package hook runs during builds.

fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs")
        .flag_if_supported("-utf-8")
        .compile("tree-sitter-sas");

    for path in ["src/parser.c", "src/scanner.c", "src/tree_sitter/parser.h"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
