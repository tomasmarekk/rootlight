//! Native binding for the separately generated finite Chapel grammar.
//! Generated C, its adjacent header and node schema must be qualified together.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_Chapel() -> *const ();
}

/// Statically linked Chapel grammar, with no parsing or source acquisition.
// SAFETY: the generated parser exports the matching static TSLanguage function.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_Chapel) };

/// Node schema from the same generation as the linked native grammar.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
