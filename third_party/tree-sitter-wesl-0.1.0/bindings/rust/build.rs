//! Compile only retained WESL parser and scanner sources.
//! Acquisition, generation and upstream lifecycle hooks are separate inputs.

fn main() {
    let mut builder = cc::Build::new();
    builder
        .std("c11")
        .include("src")
        .define("kNumXIDStartRanges", "rootlight_wesl_kNumXIDStartRanges")
        .define(
            "kNumXIDContinueRanges",
            "rootlight_wesl_kNumXIDContinueRanges",
        )
        .file("src/parser.c")
        .file("src/scanner.c");
    #[cfg(target_env = "msvc")]
    builder.flag("-utf-8");
    builder.compile("tree-sitter-wesl");
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
