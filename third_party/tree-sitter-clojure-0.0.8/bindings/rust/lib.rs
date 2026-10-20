//! Static Clojure reader tables from retained generated source.
//! Reader acceptance does not establish macro expansion or runtime bindings.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_clojure() -> *const ();
}

/// Provides the immutable ABI13 language table for the shared checked runtime.
// SAFETY: the paired retained parser exports this exact factory and returns an
// immutable process-lifetime language table; the consumer checks its ABI.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_clojure) };

/// Retains the original named-node metadata for source capture validation.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
