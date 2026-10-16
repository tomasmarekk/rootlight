//! Static language handle for authenticated V syntax.
//! Parsing does not execute compile-time code or infer types and bindings.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_v() -> *const ();
}

/// Provides the process-lifetime generated ABI15 V language table.
// SAFETY: The authenticated parser factory returns a static TSLanguage table.
// It has no external scanner; loading remains subject to the runtime ABI check.
pub const LANGUAGE_V: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_v) };

/// Describes the exact generated named nodes and source-written fields.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
