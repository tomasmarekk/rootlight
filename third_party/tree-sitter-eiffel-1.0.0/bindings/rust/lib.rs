//! Static Eiffel syntax tables from an authenticated upstream revision.
//! Parsing does not execute Eiffel routines, contracts, agents or externals.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_eiffel() -> *const ();
}

/// Provides the process-lifetime generated ABI15 Eiffel language table.
// SAFETY: The retained parser factory returns a static TSLanguage with a stateless
// operator scanner. Loading still checks the runtime's compatible ABI range.
pub const LANGUAGE_EIFFEL: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_eiffel) };

/// Describes the named syntax nodes in the pinned generated parser.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
