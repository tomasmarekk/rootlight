//! Audited Tree-sitter grammars behind Rootlight's parser-independent SDK.
//!
//! The public surface exposes checked Rootlight metadata only. Native parser
//! values remain private to this crate so grammar upgrades cannot leak into IR.

#![forbid(unsafe_code)]

mod config;
mod incremental;
mod pool;
mod registry;
mod runtime;

pub use config::{ParserSettings, RuntimeConfig, RuntimeConfigError};
pub use incremental::{
    ParseReuseKey, ParseWithPrevious, PreviousParse, ReuseInvalidation, ReuseStatus, SourceEdit,
    SourceEditError, SourceEditIdentity,
};
pub use registry::{GrammarDescriptor, GrammarFamily, GrammarRegistry, RegistryError};
pub use runtime::{CacheStats, RuntimeStats, TreeSitterProvider};
