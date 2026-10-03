//! Compiles retained ABI14 tables with immutable fixed/free scanner profiles.
//! Distinct wrapper filenames avoid duplicate native debug objects on MSVC.

fn main() {
    for free in [false, true] {
        let mut build = cc::Build::new();
        build.std("c11").include("src");
        if free {
            build
                .file("bindings/native/free-scanner.c")
                .file("bindings/native/free-parser.c")
                .define("ROOTLIGHT_COBOL_FREE_FORMAT", "1");
            for suffix in [
                "",
                "_external_scanner_create",
                "_external_scanner_destroy",
                "_external_scanner_scan",
                "_external_scanner_serialize",
                "_external_scanner_deserialize",
            ] {
                build.define(
                    &format!("tree_sitter_COBOL{suffix}"),
                    format!("tree_sitter_COBOL_free{suffix}").as_str(),
                );
            }
        } else {
            build.file("src/scanner.c").file("src/parser.c");
        }
        build.compile(if free {
            "tree-sitter-cobol-free"
        } else {
            "tree-sitter-cobol-fixed"
        });
    }
    for path in [
        "src/scanner.c",
        "src/parser.c",
        "src/tree_sitter/parser.h",
        "bindings/native/free-parser.c",
        "bindings/native/free-scanner.c",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}
