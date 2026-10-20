//! Native Mojo tables and matching metadata from the reviewed local grammar.
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_mojo() -> *const ();
}

/// The selected native language without build-time acquisition or generation.
// SAFETY: the retained generated parser exports this C language entry point.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_mojo) };

/// Native node metadata matching the selected generated parser tables.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
