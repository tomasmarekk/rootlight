//! Static language handle for the pinned CMake written-syntax grammar.
//! Parsing does not evaluate commands, variables, macros or build configuration.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_cmake() -> *const ();
}

/// Provides the immutable process-lifetime generated ABI14 CMake language table.
// SAFETY: src/parser.c defines this exact factory returning a static TSLanguage.
// The reviewed scanner is paired with its external token ordinals; loading the
// LanguageFn remains subject to the runtime's ABI compatibility check.
pub const LANGUAGE_CMAKE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_cmake) };

/// Describes the exact generated named nodes; this grammar declares no fields.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
