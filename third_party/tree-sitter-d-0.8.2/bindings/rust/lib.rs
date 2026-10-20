//! D tables and stateless scanner from one immutable upstream commit.
use tree_sitter_language::LanguageFn;
unsafe extern "C" {
    fn tree_sitter_d() -> *const ();
}
/// The selected native D grammar, without source acquisition during build.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_d) };
/// Node metadata belonging to the same selected parser tables.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
