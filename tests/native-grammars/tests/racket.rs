//! Exercise the local Racket binding through the isolated native test workspace.
//! The package's parser regressions share the same retained source and runtime.

use tree_sitter_racket::LANGUAGE;

#[path = "../../../third_party/tree-sitter-racket-0.25.0/bindings/rust/tests.rs"]
mod parser_contract;
