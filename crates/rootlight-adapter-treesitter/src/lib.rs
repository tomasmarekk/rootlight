//! Audited Tree-sitter grammars behind Rootlight's parser-independent SDK.
//!
//! The public surface exposes checked Rootlight metadata only. Native parser
//! values remain private to this crate so grammar upgrades cannot leak into IR.

#![forbid(unsafe_code)]

mod config;
mod incremental;
mod lowering;
mod pool;
mod query_pack;
mod registry;
mod runtime;

pub use config::{ParserSettings, RuntimeConfig, RuntimeConfigError};
pub use incremental::{
    ParseReuseKey, ParseWithPrevious, PreviousParse, ReuseInvalidation, ReuseStatus, SourceEdit,
    SourceEditError, SourceEditIdentity,
};
pub use lowering::{TreeSitterAnalyzer, TreeSitterAnalyzerConfigError};
pub use registry::{GrammarDescriptor, GrammarFamily, GrammarRegistry, RegistryError};
pub use runtime::{CacheStats, RuntimeStats, TreeSitterProvider};

/// Exact adapter crate version compiled into this runtime.
pub const ADAPTER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exact pinned Tree-sitter runtime crate version.
///
/// The workspace dependency is exact-pinned; keeping this adjacent to the
/// adapter API makes evidence producers record the runtime they actually use.
pub const TREE_SITTER_RUNTIME_VERSION: &str = "0.26.11";
