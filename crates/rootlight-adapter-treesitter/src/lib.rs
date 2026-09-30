//! Native grammars and closed first-party syntax behind the parser-independent SDK.
//!
//! The public surface exposes checked Rootlight metadata only. Native parser
//! values remain private to this crate so grammar upgrades cannot leak into IR.

#![forbid(unsafe_code)]

mod config;
mod groovy_bindings;
mod groovy_callables;
mod groovy_calls;
mod groovy_members;
mod incremental;
mod lowering;
mod lua_bindings;
mod markdown_bindings;
mod nix_attributes;
mod nix_bindings;
mod perl_bindings;
mod perl_code_values;
mod pool;
mod query_pack;
mod registry;
mod runtime;
mod vba_bindings;
mod vba_pointer;

pub use config::{ParserSettings, RuntimeConfig, RuntimeConfigError};
pub use incremental::{
    ParseReuseKey, ParseWithPrevious, PreviousParse, ReuseInvalidation, ReuseStatus, SourceEdit,
    SourceEditError, SourceEditIdentity,
};
pub use lowering::{
    TreeSitterAnalyzer, TreeSitterAnalyzerConfigError, TreeSitterStructuralArtifact,
};
pub use registry::{GrammarDescriptor, GrammarFamily, GrammarRegistry, RegistryError};
pub use rootlight_adapter_sdk::{
    structural_captured_name, structural_entity_kind, structural_entity_kind_from_source,
    structural_syntax_fact_order,
};
pub use runtime::{CacheStats, RuntimeStats, TreeSitterProvider};

/// Exact adapter crate version compiled into this runtime.
pub const ADAPTER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exact pinned Tree-sitter runtime crate version.
///
/// The workspace dependency is exact-pinned; keeping this adjacent to the
/// adapter API makes evidence producers record the runtime they actually use.
pub const TREE_SITTER_RUNTIME_VERSION: &str = "0.26.11";

/// Revision of the first-party, whole-module procedural VBA parser route.
///
/// Unsupported modules still use the retained native grammar. This revision
/// identifies only the closed first-party syntax inventory, not full VBA support.
pub const VBA_PROCEDURAL_FRONTEND_VERSION: &str = "rootlight-vba-procedural-1";

/// Returns the source-derived identity of the first-party VBA route.
///
/// Consumers combine this digest with the retained grammar identity while both
/// routes exist. It invalidates persisted parser artifacts when a reviewed
/// local parser input changes; it is not a hash of an installed executable.
#[must_use]
pub fn vba_procedural_frontend_digest() -> rootlight_ids::ContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rootlight.vba.procedural-frontend/1\0");
    for source in [
        VBA_PROCEDURAL_FRONTEND_VERSION.as_bytes(),
        include_bytes!("runtime.rs").as_slice(),
        include_bytes!("runtime/vba.rs").as_slice(),
        include_bytes!("../../rootlight-vba/src/lib.rs").as_slice(),
        include_bytes!("../../rootlight-vba/src/module.rs").as_slice(),
        include_bytes!("../../rootlight-vba/src/module_body.rs").as_slice(),
        include_bytes!("../../rootlight-vba/src/procedure.rs").as_slice(),
        include_bytes!("../../rootlight-vba/src/with_blocks.rs").as_slice(),
        include_bytes!("../../rootlight-vba/src/group.rs").as_slice(),
    ] {
        hasher.update(blake3::hash(source).as_bytes());
    }
    rootlight_ids::ContentHash::from_bytes(*hasher.finalize().as_bytes())
}
