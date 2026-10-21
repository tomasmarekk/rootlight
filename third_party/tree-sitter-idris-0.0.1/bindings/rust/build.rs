//! Compile retained generated tables and the checked local scanner without fetching.
fn main() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include(root.join("upstream/src"))
        .include(root.join("active-src"));
    for path in [
        "upstream/src/parser.c",
        "active-src/scanner.c",
        "active-src/rootlight_idris_status.c",
    ] {
        build.file(root.join(path));
        println!("cargo:rerun-if-changed={path}");
    }
    for path in [
        "upstream/src/tree_sitter/parser.h",
        "upstream/src/unicode.h",
        "active-src/rootlight_idris_status.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    build.compile("tree_sitter_idris");
}
