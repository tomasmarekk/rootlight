//! Static ABAP structure syntax handle for the retained native parser and scanner.
//! Generic statement nodes do not establish expression or binding semantics.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_abap() -> *const ();
}

/// Provides the immutable ABI15 ABAP table through its actual native factory.
// SAFETY: the retained parser exports this exact factory and returns a static
// language table; its paired scanner is compiled into the same local library.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_abap) };

/// Retains the upstream named-node inventory without inferring statement semantics.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
