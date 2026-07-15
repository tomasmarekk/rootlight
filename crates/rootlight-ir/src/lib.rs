//! Cross-language contract primitives for Rootlight's normalized IR.
//!
//! P0 intentionally defines only the version boundary; entity and relation
//! semantics remain owned by their later roadmap tasks.

#![forbid(unsafe_code)]

/// The initial production IR contract version.
pub const IR_VERSION: &str = "1.0";
