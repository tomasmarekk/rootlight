//! Static native DQL syntax handle for the retained local grammar.
//! Syntax ownership and physical spans do not establish query execution semantics.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_dql() -> *const ();
}

/// Provides the immutable, locally generated ABI14 native DQL grammar table.
// SAFETY: this crate compiles the retained C factory returning a process-lifetime
// static language table and stateless scanner, without dynamically loaded code.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_dql) };
