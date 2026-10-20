//! Retained native webassembly parser tables and matching node metadata.
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_wat() -> *const ();
}

/// The pinned C language entry point.
// SAFETY: the retained parser exports this exact C ABI function.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_wat) };

/// Node metadata from the unchanged generated parser source.
pub const NODE_TYPES: &str = include_str!("../../upstream/src/node-types.json");
