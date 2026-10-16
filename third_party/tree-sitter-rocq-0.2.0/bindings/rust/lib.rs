//! Static language handle for authenticated Rocq syntax tables.
//! Native syntax does not establish proof checking or complete command semantics.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_rocq() -> *const ();
}

/// Provides the process-lifetime generated ABI15 Rocq language table.
// SAFETY: The authenticated C factory returns its static TSLanguage table and
// has no external scanner. The parser runtime still checks ABI compatibility.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_rocq) };

/// Describes the unchanged generated named nodes and authored source fields.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
