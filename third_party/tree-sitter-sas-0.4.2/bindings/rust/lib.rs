//! Static language handle for the pinned SAS written-source grammar.
//! Native syntax is not SAS compiler acceptance, macro expansion or runtime binding.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_sas() -> *const ();
}

/// Provides the process-lifetime native language table for runtime ABI validation.
// SAFETY: The retained parser.c factory returns its immutable static TSLanguage
// through this exact C symbol; LanguageFn does not reinterpret a Rust Language.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_sas) };

/// Describes the exact named native nodes and fields in the retained static table.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
