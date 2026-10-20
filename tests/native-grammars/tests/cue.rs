//! Exercise the local CUE binding from the isolated native grammar workspace.
//! The shared parser contract uses complete retained C sources and physical input.

use tree_sitter_cue::LANGUAGE;

#[path = "../../../third_party/tree-sitter-cue-0.2.0/bindings/rust/tests.rs"]
mod parser_contract;
