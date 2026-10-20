//! Statically linked Ada grammar from the complete pinned upstream source tree.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_ada() -> *const ();
}

/// Returns the linked immutable grammar through Tree-sitter's language ABI.
// SAFETY: the bundled generated C function returns its process-lifetime static
// language table, whose ABI is independently admitted by the registry.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_ada) };

/// Native node declarations retained exactly from the pinned grammar.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
