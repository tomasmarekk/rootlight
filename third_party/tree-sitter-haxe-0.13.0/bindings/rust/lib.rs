//! Local ABI wrapper for a complete pinned Haxe grammar source tree.
//! Active generated sources include the reviewed authored grammar corrections.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_haxe() -> *const ();
}

/// The statically linked ABI15 Haxe parser entry point.
pub const LANGUAGE: LanguageFn = unsafe {
    // SAFETY: the retained generated C parser exports this immutable grammar ABI.
    LanguageFn::from_raw(tree_sitter_haxe)
};

/// Node types for the active corrected parser, rather than the inert mirror.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
