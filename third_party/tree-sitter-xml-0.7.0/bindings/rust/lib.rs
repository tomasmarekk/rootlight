//! Static language handles for the pinned XML and DTD written-syntax grammars.
//! Parsing does not establish XML validity, expanded entity values, external
//! subset availability, namespace identity or DTD declaration resolution.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_xml() -> *const ();
    fn tree_sitter_dtd() -> *const ();
}

/// Provides the immutable process-lifetime locally generated ABI14 XML table.
// SAFETY: xml/src/parser.c defines this exact C factory and returns its immutable
// static TSLanguage table. LanguageFn retains the factory; the runtime checks
// ABI compatibility when loading it. The paired stateless scanner is linked
// from the retained preferred xml/src/scanner.c and common/scanner.h.
pub const LANGUAGE_XML: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_xml) };

/// Provides the immutable process-lifetime locally generated ABI14 DTD table.
// SAFETY: dtd/src/parser.c defines this exact C factory and returns its immutable
// static TSLanguage table. LanguageFn retains the factory; the runtime checks
// ABI compatibility when loading it. The paired dtd/src/scanner.c and shared
// preferred common/scanner.h are linked with this table, not the XML factory.
pub const LANGUAGE_DTD: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_dtd) };

/// Describes the exact generated XML named nodes and physical fields.
pub const XML_NODE_TYPES: &str = include_str!("../../xml/src/node-types.json");

/// Describes the exact generated DTD named nodes and physical fields.
pub const DTD_NODE_TYPES: &str = include_str!("../../dtd/src/node-types.json");

/// Retains the original XML highlighting query without claiming SDK qualification.
pub const XML_HIGHLIGHTS_QUERY: &str = include_str!("../../queries/xml/highlights.scm");

/// Retains the original DTD highlighting query without claiming SDK qualification.
pub const DTD_HIGHLIGHTS_QUERY: &str = include_str!("../../queries/dtd/highlights.scm");
