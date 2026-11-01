//! Static handle for the retained Turtle/TriG native grammar.
//! Loading this parser does not qualify RDF semantics or production indexing.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_turtle() -> *const ();
}

/// Provides the immutable generated ABI14 grammar table.
// SAFETY: the retained C factory returns a process-lifetime static language table
// compiled with its stateless scanner by this crate's local build script.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_turtle) };
