//! Static native N-Quads syntax handle for the locally maintained grammar.
//! Loading a grammar does not establish dataset semantics or product indexing.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_nquads() -> *const ();
}

/// Provides the immutable, locally generated ABI14 N-Quads grammar table.
// SAFETY: this crate compiles the retained C factory returning a process-lifetime
// static language table and stateless scanner, with no dynamically loaded code.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_nquads) };
