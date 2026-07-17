//! Audited Tree-sitter grammars behind Rootlight's parser-independent SDK.
//!
//! The public surface exposes checked Rootlight metadata only. Native parser
//! values remain private to this crate so grammar upgrades cannot leak into IR.

#![forbid(unsafe_code)]

mod registry;

pub use registry::{GrammarDescriptor, GrammarFamily, GrammarRegistry, RegistryError};
