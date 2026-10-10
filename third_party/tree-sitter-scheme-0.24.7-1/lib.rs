//! Static binding for Rootlight's retained default Scheme reader.
//! This does not select, compile, or qualify any dialect grammar.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_scheme() -> *const ();
}

/// Immutable default-root ABI-15 language table from the authenticated parser.
// SAFETY: the linked generated function returns its immutable static
// TSLanguage table from the retained ABI-15 default parser.
pub const LANGUAGE_SCHEME: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_scheme) };
