//! Compile complete retained inputs without source acquisition or generation.
fn main() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for path in [
        "upstream/src/parser.c",
        "upstream/src/tree_sitter/parser.h",
        "upstream/src/tree_sitter/alloc.h",
        "src/scanner.c",
        "src/rootlight_lean_status.c",
        "src/rootlight_lean_status.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    cc::Build::new()
        .std("c11")
        .include(root.join("upstream/src"))
        .include(root.join("src"))
        .file(root.join("upstream/src/parser.c"))
        .file(root.join("src/scanner.c"))
        .file(root.join("src/rootlight_lean_status.c"))
        .compile("tree_sitter_lean");
}
