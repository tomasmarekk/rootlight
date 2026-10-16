//! Static language handle for authenticated Verilog and SystemVerilog syntax.
//! Parsing does not elaborate designs or resolve macro and include environments.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_systemverilog() -> *const ();
}

/// Provides the process-lifetime generated ABI15 SystemVerilog language table.
// SAFETY: The authenticated src/parser.c factory returns a static TSLanguage.
// It has no external scanner; loading remains subject to the runtime ABI check.
pub const LANGUAGE_SYSTEMVERILOG: LanguageFn =
    unsafe { LanguageFn::from_raw(tree_sitter_systemverilog) };

/// Describes the exact generated named nodes and written declaration fields.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
