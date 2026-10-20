//! Exercise the retained Pony parser/scanner through the isolated workspace.
//! These controls share the same native inputs and declared runtime as the binding.

use tree_sitter_pony::LANGUAGE;

#[path = "../../../third_party/tree-sitter-pony-0.0.3/bindings/rust/tests.rs"]
mod parser_contract;
