//! Static Crystal syntax tables and stateful literal/heredoc scanner.
//! This binding performs no generation, acquisition or semantic evaluation.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_crystal() -> *const ();
}

/// Provides the retained process-lifetime ABI15 grammar factory.
// SAFETY: The compiled factory returns a static TSLanguage; Tree-sitter checks
// ABI compatibility and owns each parser's scanner lifetime.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_crystal) };

/// Named node metadata generated together with the selected parser tables.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

#[cfg(test)]
mod tests;
