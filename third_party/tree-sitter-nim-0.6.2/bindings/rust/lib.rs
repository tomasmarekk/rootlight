//! Static Nim ABI14 tables paired with their retained external scanner.
//! Syntax acceptance does not establish compiler selection or runtime semantics.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_nim() -> *const ();
}

/// Provides the immutable native language table for the checked shared runtime.
// SAFETY: the retained generated parser exports this factory with a process-lifetime
// immutable language table; the shared runtime separately checks its ABI.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_nim) };

/// Retains the original named-node metadata used to validate structural captures.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
