//! Compile complete retained BRE/ERE tables and the qualified scoped scanner.
//! Both entry points stay explicit; build-time generation and fetching are absent.
fn main() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let files = [
        "upstream/src/parser.c",
        "upstream/gsed_ere/src/parser.c",
        "active-src/src/scanner.c",
        "active-src/gsed_ere/src/scanner.c",
        "active-src/common/rootlight_sed_status.c",
    ];
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include(root.join("upstream/src"))
        .include(root.join("upstream/gsed_ere/src"))
        .include(root.join("active-src/common"));
    for path in files {
        build.file(root.join(path));
        println!("cargo:rerun-if-changed={path}");
    }
    for path in [
        "upstream/src/tree_sitter/parser.h",
        "upstream/gsed_ere/src/tree_sitter/parser.h",
        "active-src/src/tree_sitter/parser.h",
        "active-src/common/scanner.h",
        "active-src/common/tokens.h",
        "active-src/common/rootlight_sed_status.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    build.compile("tree_sitter_gsed");
}
