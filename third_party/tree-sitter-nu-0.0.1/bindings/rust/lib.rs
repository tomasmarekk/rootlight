//! Static native handle for the retained Nushell grammar.
//! Only written syntax is parsed; Nu commands, plugins and completion functions
//! are never executed by this binding.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_nu() -> *const ();
}

/// Provides the immutable process-lifetime generated ABI15 grammar table.
// SAFETY: the retained parser defines this factory and returns its static
// TSLanguage; the paired retained scanner is linked by the local build script.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_nu) };
