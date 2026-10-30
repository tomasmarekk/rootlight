//! Static handle for the retained WESL parser and external scanner.
//! Syntax parsing never resolves imports or executes shader source.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_wesl() -> *const ();
}

/// Provides the immutable generated ABI15 grammar table.
// SAFETY: the retained C factory returns its process-lifetime static language
// table, paired with the unchanged scanner by this crate's local build script.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_wesl) };
