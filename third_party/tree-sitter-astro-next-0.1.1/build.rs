// Rootlight modifications track all patched native scanner inputs.
// This build compiles the pinned C parser without runtime downloads.
fn main() {
    let src_dir = std::path::Path::new("src");

    let mut c_config = cc::Build::new();
    c_config.std("c11").include(src_dir);

    #[cfg(target_env = "msvc")]
    c_config.flag("-utf-8");

    let parser_path = src_dir.join("parser.c");
    c_config.file(&parser_path);
    println!("cargo:rerun-if-changed={}", parser_path.to_str().unwrap());

    let scanner_path = src_dir.join("scanner.c");
    c_config.file(&scanner_path);
    println!("cargo:rerun-if-changed={}", scanner_path.to_str().unwrap());
    println!("cargo:rerun-if-changed=src/state.h");
    println!("cargo:rerun-if-changed=src/name.h");
    println!("cargo:rerun-if-changed=src/template.h");
    println!("cargo:rerun-if-changed=src/javascript.h");
    println!("cargo:rerun-if-changed=src/tag.h");

    c_config.compile("tree-sitter-astro");
}
