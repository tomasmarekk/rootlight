//! Static handle for the unchanged retained RON parser and external scanner.
//! Loading this grammar does not qualify production indexing or schema semantics.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_ron() -> *const ();
}

/// Provides the immutable generated ABI14 grammar table.
// SAFETY: the retained C factory returns a process-lifetime static language table,
// paired with its unchanged external scanner by this crate's local build script.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_ron) };
