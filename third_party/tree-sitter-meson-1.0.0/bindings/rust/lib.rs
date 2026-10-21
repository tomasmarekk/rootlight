//! Retained native Meson tables and their matching node metadata.
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_meson() -> *const ();
}

/// The pinned C language entry point.
// SAFETY: the unchanged retained parser exports this exact C ABI function.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_meson) };

/// Node metadata from the same generated source as LANGUAGE.
pub const NODE_TYPES: &str = include_str!("../../upstream/src/node-types.json");
