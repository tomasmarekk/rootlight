//! Static protobuf syntax tables from a pinned upstream revision.
//! This binding performs no source acquisition, generation or language evaluation.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_proto() -> *const ();
}

/// Provides the process-lifetime generated ABI14 language table.
// SAFETY: The unchanged C factory returns its static TSLanguage and has no
// external scanner. The consumer must still enforce the runtime ABI range.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_proto) };

/// Describes the named nodes of the same pinned generated parser.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
