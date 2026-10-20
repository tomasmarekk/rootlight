//! Native Ring tables and scanner from one pinned upstream revision.
//! The scanner reset correction is recorded in the package provenance.
use tree_sitter_language::LanguageFn;
unsafe extern "C" {
    fn tree_sitter_ring() -> *const ();
}
/// The selected Ring grammar without acquisition or generation during build.
// SAFETY: the statically linked generated parser exports this C language entry.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_ring) };
/// Native node metadata belonging to the selected parser tables.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
