//! Static Racket syntax tables and the local scalar delimiter correction.
//! Reader extensions and semantic binding remain responsibilities of the caller.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_racket() -> *const ();
}

/// Provides the process-lifetime ABI15 language table and external scanner.
// SAFETY: The retained factory returns a static TSLanguage; the runtime checks
// ABI compatibility and owns each parser's separate scanner lifecycle.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_racket) };

/// Named nodes of the same retained generated parser.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

#[cfg(test)]
mod tests;
