//! Static VB.NET syntax tables with physical final-declaration support.
//! Parsing does not load assemblies, evaluate code, or invoke a compiler.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_vbnet() -> *const ();
}

/// Provides the process-lifetime generated ABI14 VB.NET language table.
// SAFETY: The retained generated factory returns one static TSLanguage. The
// adapter still checks that ABI14 is within the active runtime's supported range.
pub const LANGUAGE_VBNET: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_vbnet) };

/// Describes the named syntax nodes in the retained generated parser.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
