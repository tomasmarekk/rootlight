//! Static Zig syntax tables; this grammar has no external scanner.
//! Compilation consumes only retained parser and header bytes.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_zig() -> *const ();
}

/// Provides the retained process-lifetime ABI14 grammar factory.
// SAFETY: The compiled factory returns a static TSLanguage. Tree-sitter checks
// ABI compatibility before it admits the returned language to a parser.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_zig) };

/// Named node metadata generated with the selected syntax tables.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

#[cfg(test)]
mod tests;
