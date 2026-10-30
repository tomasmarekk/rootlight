//! Static handle for the unchanged retained TypeSpec parser.
//! Grammar loading does not qualify production indexing or TypeSpec resolution.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_typespec() -> *const ();
}

/// Provides the immutable generated ABI15 grammar table.
// SAFETY: the retained C factory returns a process-lifetime static language table
// compiled unchanged by this crate's local build script; no scanner is required.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_typespec) };
