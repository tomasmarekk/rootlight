//! Static syntax handle for the retained native VB.NET parser.
//! Syntax structure alone does not establish dispatch or type bindings.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_vb_dotnet() -> *const ();
}

/// Provides the immutable ABI15 table through its actual native factory.
// SAFETY: the retained generated C exports this factory with a static language
// table, and is linked with its paired headers by the local build script.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_vb_dotnet) };

/// Retains the upstream named-node inventory for native structural queries.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
