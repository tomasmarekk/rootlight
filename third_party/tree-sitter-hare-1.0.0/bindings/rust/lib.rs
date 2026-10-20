//! One retained native Hare grammar, independent of the old upstream runtime.
use tree_sitter_language::LanguageFn;
unsafe extern "C" {
    fn tree_sitter_hare() -> *const ();
}
/// The selected grammar without acquisition or generation during compilation.
// SAFETY: the generated parser statically exports this C language entry.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_hare) };
/// Native node metadata belonging to the exact parser tables.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
