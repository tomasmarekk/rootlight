//! Apex-only static binding for the pinned sfapex source revision.
//! The original module API is retained without exposing the package's other
//! three parsers or claiming Salesforce compiler and runtime semantics.

/// Exposes the supplied Apex native table and its exact node metadata.
pub mod apex {
    use tree_sitter_language::LanguageFn;

    unsafe extern "C" {
        fn tree_sitter_apex() -> *const ();
    }

    /// Provides the process-lifetime ABI15 table from the supplied Apex C source.
    // SAFETY: the authenticated apex/src/parser.c defines this exact C factory,
    // which returns an immutable static TSLanguage with process lifetime.
    // LanguageFn retains the factory and the runtime checks ABI compatibility.
    pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_apex) };

    /// Describes the exact supplied named Apex nodes and their native fields.
    pub const NODE_TYPES: &str = include_str!("../apex/src/node-types.json");
}
