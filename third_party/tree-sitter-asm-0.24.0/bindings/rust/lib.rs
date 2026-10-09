//! Static language handle for the pinned, locally patched Assembly grammar.
//! Physical written syntax does not establish assembler, macro-expansion,
//! linker, case-folded symbol or runtime identities.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_asm() -> *const ();
}

/// Provides the process-lifetime immutable locally generated ABI14 table.
// SAFETY: src/parser.c defines this exact C factory and returns its immutable
// static TSLanguage table. LanguageFn retains the factory; the runtime checks
// ABI compatibility when loading it.
pub const LANGUAGE_ASSEMBLY: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_asm) };

/// Describes the exact locally generated named nodes and physical fields.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
