//! Static GraphQL language table from the exact retained generated-C source.
//! Source structure does not establish schema validity, execution or complete dialect support.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_graphql() -> *const ();
}

/// Returns the process-lifetime immutable ABI14 grammar through the reviewed factory.
// SAFETY: the paired retained src/parser.c exports exactly this factory and returns
// its immutable static TSLanguage table. The shared runtime checks ABI compatibility.
pub const LANGUAGE_GRAPHQL: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_graphql) };

/// Retains native named-node metadata without inventing absent field labels.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
