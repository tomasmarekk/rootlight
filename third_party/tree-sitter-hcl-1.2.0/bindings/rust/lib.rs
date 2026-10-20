//! Pinned HCL tables and checked scanner state, compiled from local C inputs.
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_hcl() -> *const ();
}

/// The selected native HCL grammar.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_hcl) };
/// Node metadata for the same selected parser tables.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
