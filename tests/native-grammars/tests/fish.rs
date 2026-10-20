//! Exercise the local Fish binding in the isolated native grammar workspace.
//! These controls use the same retained parser/scanner and declared runtime.

use tree_sitter_fish::LANGUAGE;

#[path = "../../../third_party/tree-sitter-fish-3.7.0/bindings/rust/tests.rs"]
mod parser_contract;
