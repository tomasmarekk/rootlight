//! Static handle for the retained native Notation3 grammar.
//! Syntax loading alone does not qualify production indexing or logical inference.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_n3() -> *const ();
}

/// Provides the immutable generated ABI15 grammar table.
// SAFETY: the retained C factory returns a process-lifetime static language table
// compiled by this crate's local build script, without an external scanner.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_n3) };
