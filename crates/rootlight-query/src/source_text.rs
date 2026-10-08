//! Lossless Unicode results retaining original-byte source authority.
//!
//! The source service owns validation and decoded buffers. Query projection
//! borrows presentation text for accounting instead of serializing raw bytes twice.

use rootlight_ids::GenerationId;
use rootlight_ir::SourceRef;
use rootlight_source::{SourceChunk, SourceTextChunk};
use rootlight_vfs::TextEncoding;
use serde::{Serialize, Serializer};

use crate::{ConditionalActivityAssessment, ExecutionCompleteness, RepositoryDataTrust};

/// A validated text chunk carrying physical identity and query trust metadata.
#[derive(Debug, PartialEq, Eq)]
pub struct QuerySourceTextChunk {
    source: SourceTextChunk,
}

impl QuerySourceTextChunk {
    pub(crate) const fn new(source: SourceTextChunk) -> Self {
        Self { source }
    }

    /// Borrows exact original bytes and generation-owned metadata.
    #[must_use]
    pub const fn raw(&self) -> &SourceChunk {
        self.source.raw()
    }

    /// Borrows unnormalized Unicode text, preserving selected BOM and line endings.
    #[must_use]
    pub fn text(&self) -> &str {
        self.source.text()
    }

    /// Returns the physical encoding verified across the whole original file.
    #[must_use]
    pub const fn physical_encoding(&self) -> TextEncoding {
        self.source.physical_encoding()
    }

    /// Borrows the immediate container dependency for a virtual extracted source.
    #[must_use]
    pub fn extracted_from(&self) -> Option<&SourceRef> {
        self.raw().extracted_from.as_ref()
    }

    /// Returns the mandatory untrusted-repository-data label.
    #[must_use]
    pub const fn trust(&self) -> RepositoryDataTrust {
        RepositoryDataTrust::UntrustedRepositoryData
    }

    /// Moves the validated source owner and its fixed-size container identity.
    #[must_use]
    pub fn into_parts(self) -> (SourceTextChunk, Option<SourceRef>) {
        let extracted_from = self.source.raw().extracted_from.clone();
        (self.source, extracted_from)
    }
}

impl Serialize for QuerySourceTextChunk {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct Presentation<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            extracted_from: Option<&'a SourceRef>,
            reference: &'a SourceRef,
            path: &'a str,
            start_byte: u64,
            end_byte: u64,
            start_line: Option<u64>,
            end_line: Option<u64>,
            content: &'a str,
            physical_encoding: &'static str,
            content_hash: rootlight_ids::ContentHash,
            language: &'a str,
            generated: bool,
            trust: RepositoryDataTrust,
        }

        let raw = self.raw();
        Presentation {
            extracted_from: self.extracted_from(),
            reference: &raw.reference,
            path: &raw.path,
            start_byte: raw.start_byte,
            end_byte: raw.end_byte,
            start_line: raw.start_line,
            end_line: raw.end_line,
            content: self.text(),
            physical_encoding: self.physical_encoding().as_str(),
            content_hash: raw.content_hash,
            language: &raw.language,
            generated: raw.generated,
            trust: self.trust(),
        }
        .serialize(serializer)
    }
}

/// Lossless source presentation with separate original-byte and text accounting.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct SourceTextReadQueryResult {
    /// Immutable generation that served the read.
    pub generation: GenerationId,
    /// Verified text chunks in selector order, including duplicate selections.
    pub chunks: Vec<QuerySourceTextChunk>,
    /// Aggregate selected original-byte widths, not UTF-8 presentation lengths.
    pub total_source_bytes: usize,
    /// Aggregate UTF-8 presentation lengths before transport serialization.
    pub total_text_bytes: usize,
    /// Conditional membership of actual expanded original-byte ranges.
    pub conditional_activity: Vec<ConditionalActivityAssessment>,
    /// Authoritative completeness, including correctness-bearing metadata scans.
    pub execution: ExecutionCompleteness,
}

/// A bounded presentation prefix retaining each complete approved selector.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct SourceTextPrefixReadQueryResult {
    /// Text and conditional evidence for the actual returned physical ranges.
    pub result: SourceTextReadQueryResult,
    /// Whether a text ceiling intentionally clipped any expanded context range.
    /// This is independent of resource-limited execution completeness.
    pub prefix_truncated: bool,
}
