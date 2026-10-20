//! Reviewed official Flux AST/parser sources with typed cooperative controls.
//! Native syntax diagnostics and unsupported validation remain distinct from stops.

#[macro_use]
extern crate serde_derive;
pub mod ast;
pub mod parser;
pub mod scanner;
pub mod control;
pub mod controlled_parser;
type DefaultHasher = std::hash::BuildHasherDefault<fnv::FnvHasher>;
