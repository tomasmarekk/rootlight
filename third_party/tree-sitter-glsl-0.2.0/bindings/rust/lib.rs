//! Static language handle for the pinned GLSL grammar with repaired precision.
//! Physical syntax does not prove shader-profile validity, preprocessor
//! selection, linked interface identity or runtime binding.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_glsl() -> *const ();
}

/// Provides the process-lifetime immutable locally generated ABI14 table.
// SAFETY: src/parser.c defines this exact C factory and returns its immutable
// static TSLanguage table. LanguageFn retains the factory; the runtime checks
// ABI compatibility when loading it.
pub const LANGUAGE_GLSL: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_glsl) };

/// Describes the exact locally generated named nodes and physical fields.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

/// Preserves the upstream highlighting query without claiming SDK qualification.
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");
