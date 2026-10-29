//! Static handle for the retained embedded-template syntax grammar.
//! Parsing never renders templates or executes embedded source code.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_embedded_template() -> *const ();
}

/// Provides the immutable generated ABI15 grammar table.
// SAFETY: the retained C factory returns its process-lifetime static language
// table and the paired local build script links that exact generated source.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_embedded_template) };
