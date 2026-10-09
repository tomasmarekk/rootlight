//! Static language handle for the pinned, locally patched PL/SQL grammar.
//! This binding exposes written syntax, not Oracle compiler or dispatch proof.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_plsql() -> *const ();
}

/// Provides the locally generated process-lifetime ABI14 language table.
// SAFETY: src/parser.c returns its immutable static TSLanguage through this
// exact C factory; LanguageFn delays conversion to the runtime's ABI check.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_plsql) };

/// Describes the exact locally generated named nodes and native fields.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
