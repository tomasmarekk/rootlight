//! Compile only the retained corrected parser, scanner and active headers.
//! Upstream mirror sources are provenance and never select the active parser.

fn main() {
    let source = std::path::Path::new("src");
    let mut build = cc::Build::new();
    build.include(source).std("c11");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.flag("/utf-8");
    }
    for file in ["parser.c", "scanner.c"] {
        let path = source.join(file);
        build.file(&path);
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=src/tree_sitter");
    build.compile("tree-sitter-haxe");
}
