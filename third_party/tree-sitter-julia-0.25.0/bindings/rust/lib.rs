//! Static language handle for authenticated Julia syntax tables and scanner.
//! Parsing does not evaluate Julia code, imports, commands or macro expansions.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_julia() -> *const ();
}

/// Provides the process-lifetime generated ABI15 Julia language table.
// SAFETY: The authenticated src/parser.c factory returns a static TSLanguage;
// its external-token ordinals match the retained scanner. Loading the handle
// remains subject to the Tree-sitter runtime's ABI compatibility check.
pub const LANGUAGE_JULIA: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_julia) };

/// Describes the exact generated named nodes and declaration-head structures.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
