//! Compiles only the retained ABI14 XML and DTD tables and paired scanners.
//! Acquisition, generation and archived upstream lifecycle hooks never run here.

fn main() {
    for language in ["xml", "dtd"] {
        let source = format!("{language}/src");
        let mut builder = cc::Build::new();
        builder
            .std("c11")
            .include(&source)
            .file(format!("{source}/parser.c"))
            .file(format!("{source}/scanner.c"));
        #[cfg(target_env = "msvc")]
        builder.flag("-utf-8");
        builder.compile(&format!("tree-sitter-{language}"));

        for path in [
            "parser.c",
            "scanner.c",
            "tree_sitter/parser.h",
            "tree_sitter/alloc.h",
            "tree_sitter/array.h",
        ] {
            println!("cargo:rerun-if-changed={source}/{path}");
        }
    }
    println!("cargo:rerun-if-changed=common/scanner.h");
}
