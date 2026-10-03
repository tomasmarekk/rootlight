//! Static language handles for explicitly selected COBOL source formats.
//! The grammar recognizes written syntax, not compiler legality or dispatch.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_COBOL() -> *const ();
    fn tree_sitter_COBOL_free() -> *const ();
}

/// Provides fixed-column scanning without rewriting source bytes.
// SAFETY: the retained C factory returns a process-lifetime ABI14 TSLanguage.
pub const FIXED: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_COBOL) };

/// Provides scanning without fixed-column filtering or continuation inference.
// SAFETY: the renamed factory returns an independent static ABI14 language table.
pub const FREE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_COBOL_free) };

/// Describes the exact named nodes and fields shared by both native profiles.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
