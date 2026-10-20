//! Reviewed native Q# grammar; all compilation inputs are local.
use tree_sitter_language::LanguageFn;
unsafe extern "C" {
    fn tree_sitter_qsharp() -> *const ();
}
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_qsharp) };
pub const NODE_TYPES: &str = include_str!("../../upstream/src/node-types.json");
