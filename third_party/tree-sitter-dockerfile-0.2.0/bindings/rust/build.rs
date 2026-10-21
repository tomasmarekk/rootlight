//! Compile retained generated tables and the checked scanner/status derivative.
//! Source acquisition and grammar generation never occur during compilation.

fn main() {
    let mut build = cc::Build::new();
    build.std("c11").include("generated/src").include("active-src")
        .file("generated/src/parser.c").file("active-src/scanner.c")
        .file("active-src/rootlight_dockerfile_status.c");
    if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|environment| environment == "msvc") {
        build.flag("/utf-8");
    }
    build.compile("tree-sitter-dockerfile");
    for path in ["generated/src/parser.c", "generated/src/tree_sitter/parser.h",
        "generated/src/tree_sitter/alloc.h", "generated/src/tree_sitter/array.h",
        "active-src/scanner.c", "active-src/rootlight_dockerfile_status.c",
        "active-src/rootlight_dockerfile_status.h"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
