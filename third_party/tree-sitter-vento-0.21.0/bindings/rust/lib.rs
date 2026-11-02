//! Static Vento syntax handle for the retained native parser and scanner.
//! Host syntax does not establish JavaScript bindings or template evaluation.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_vento() -> *const ();
}

/// Provides the immutable ABI15 Vento table through its actual native factory.
// SAFETY: the retained parser exports this exact factory and returns a static
// language table; its paired scanner is compiled into the same local library.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_vento) };

/// Retains the upstream named-node inventory without inferring JavaScript semantics.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
