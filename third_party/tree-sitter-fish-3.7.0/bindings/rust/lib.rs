//! Static Fish syntax tables and their stateless concatenation scanner.
//! Runtime expansion and command lookup remain responsibilities of the caller.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_fish() -> *const ();
}

/// Provides the process-lifetime ABI14 language table and external scanner.
// SAFETY: The retained factory returns a static TSLanguage; Tree-sitter checks
// ABI compatibility and manages each parser's scanner lifecycle.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_fish) };

/// Named nodes of the same retained generated parser.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

#[cfg(test)]
mod tests;
