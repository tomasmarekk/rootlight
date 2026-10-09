//! Static Pascal binding for locally patched published inputs of version 0.10.2.
//! Reproducibly generated tables and unchanged node metadata do not establish
//! compiler semantics, active preprocessor branches, or Rootlight integration.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_pascal() -> *const ();
}

/// Provides the process-lifetime ABI14 table from the supplied Pascal C source.
// SAFETY: the reviewed static src/parser.c defines this exact C factory, which
// returns an immutable static TSLanguage with process lifetime. LanguageFn
// retains the factory and the runtime checks native ABI compatibility.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_pascal) };

/// Describes the exact supplied named Pascal nodes and their native fields.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
