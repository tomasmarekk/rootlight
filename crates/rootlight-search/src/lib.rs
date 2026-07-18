//! Generation-pinned lexical indexing and deterministic code search.
//!
//! The crate owns Tantivy-specific details behind bounded, source-redacted
//! contracts. Generation publication remains the storage layer's responsibility.

#![forbid(unsafe_code)]

mod artifact;
mod index;
mod model;
mod tokenizer;

pub use artifact::{ArtifactBudget, LexicalArtifactManifest, VerifiedLexicalArtifact};
pub use index::{
    LexicalIndex, LexicalIndexBuilder, LexicalSearch, validate_build_admission,
    validate_search_request,
};
pub use model::{
    BuildBudget, BuildStats, DocumentField, LexicalDocument, QueryViolation, SearchBudget,
    SearchError, SearchHit, SearchMode, SearchOutcome, SearchRequest,
};
