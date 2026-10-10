//! Static language handle for the pinned WGSL grammar and hardened scanner.
//! Physical syntax does not prove shader validation, evaluated constants,
//! overload resolution, linked resource identity or runtime binding.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_wgsl() -> *const ();
}

/// Provides the process-lifetime immutable locally generated ABI14 table.
// SAFETY: src/parser.c defines this exact C factory and returns its immutable
// static TSLanguage table. LanguageFn retains the factory; the runtime checks
// ABI compatibility when loading it. The paired external scanner is linked
// from the retained locally hardened src/scanner.c.
pub const LANGUAGE_WGSL: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_wgsl) };

/// Describes the exact locally generated named nodes and physical fields.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

/// Retains the original highlighting query without claiming SDK qualification.
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");
