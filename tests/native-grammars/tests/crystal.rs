//! Exercise the selected Crystal parser/scanner in the isolated native workspace.
//! The binding and these controls consume the same complete pinned native inputs.

use tree_sitter_crystal::LANGUAGE;

#[path = "../../../third_party/tree-sitter-crystal-0.0.1/bindings/rust/tests.rs"]
mod parser_contract;
