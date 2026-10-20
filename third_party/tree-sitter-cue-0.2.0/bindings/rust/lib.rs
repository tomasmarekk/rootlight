//! Static CUE syntax tables and scanner from the complete pinned upstream source.
//! Constraint evaluation and imported bindings remain outside this native binding.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_cue() -> *const ();
}

/// Provides the process-lifetime ABI15 grammar and its raw-string scanner.
// SAFETY: The retained factory returns a static TSLanguage. Tree-sitter checks
// ABI compatibility and owns the scanner lifetime for each parser instance.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_cue) };

/// Named node metadata from the same unchanged generated parser.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

#[cfg(test)]
mod tests;
