//! Static VHDL syntax tables and the reviewed local external scanner.
//! Grammar admission does not establish dialect or semantic completeness.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_vhdl() -> *const ();
}

/// Provides the process-lifetime generated ABI15 language table and scanner.
// SAFETY: The retained factory returns a static TSLanguage. The runtime checks
// ABI compatibility; scanner state is owned separately by each parser.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_vhdl) };

/// Describes the named nodes of the same retained generated parser.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

#[cfg(test)]
mod tests;
