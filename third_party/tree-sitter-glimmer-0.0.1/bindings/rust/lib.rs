//! Static native handle for the pinned Glimmer written-template grammar.
//! Parsing preserves written block structure without helper dispatch or
//! template evaluation; matching block names remain the adapter's responsibility.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_glimmer() -> *const ();
}

/// Provides the immutable process-lifetime generated ABI15 grammar table.
// SAFETY: the retained parser.c defines this factory and returns its static
// TSLanguage; the paired stateless scanner is linked by the local build script.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_glimmer) };
