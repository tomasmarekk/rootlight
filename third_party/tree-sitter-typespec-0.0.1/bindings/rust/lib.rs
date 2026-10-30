//! Static handle for the reviewed TypeSpec parser and local grammar corrections.
//! Grammar loading does not qualify production indexing or TypeSpec resolution.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_typespec() -> *const ();
}

/// Provides the immutable generated ABI15 grammar table.
// SAFETY: the retained C factory returns a process-lifetime static language table
// compiled from reviewed local sources by the build script; no scanner is required.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_typespec) };
