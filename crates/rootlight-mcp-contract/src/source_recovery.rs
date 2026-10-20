//! Source read 1.4 retains the exact aggregate-capacity reason alongside bytes.
//! Recovery reports source-search capture, never structural or semantic support.

use rootlight_ids::FileId;
use rootlight_ir::AggregateSourceRecovery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::conditional_activity::{ConditionalActivityAssessment, ConditionalActivityError};
use crate::source_entity::{EntityReadEnvelope, EntityToolResponse};
use crate::source_text::{SourceChunkV1_3, validate_source_text_data};
use crate::vertical::{SourceElision, StaleSourceReference};

/// Current exact-source contract; the 1.3 input controls are unchanged.
pub const SOURCE_READ_RECOVERY_VERSION: &str = "1.4";

/// Exact aggregate-recovery-aware response revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SourceReadRecoveryVersion {
    /// Source read contract 1.4.
    #[serde(rename = "1.4")]
    V1_4,
}

/// Capture available to source-text search for one generation member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceTextSearchCoverage {
    /// The exact complete source projection is available for bounded text search.
    WholeSourceProjection,
    /// Only captured metadata is searchable; exact source reading is separate.
    MetadataOnlyCaptured,
    /// The source projection has no supported text encoding.
    UnsupportedEncoding,
}

/// One returned file's source-search capture, independent of analysis support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceRecoveryFile {
    /// Exact generation member identity, including derived source members.
    pub file: FileId,
    /// Searchable capture for this file; it does not claim a full vocabulary.
    pub text_search: SourceTextSearchCoverage,
}

/// One immutable generation's aggregate-capacity recovery and returned files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceReadRecovery {
    /// Durable source inventory and exact capacity admission witness.
    pub cause: AggregateSourceRecovery,
    /// Sorted unique file IDs, exactly the distinct returned chunk-file set.
    #[schemars(length(min = 1, max = 32))]
    pub files: Vec<SourceRecoveryFile>,
}

impl SourceReadRecovery {
    /// Checks the durable witness, one source context and exact returned membership.
    ///
    /// # Errors
    /// Returns [`ConditionalActivityError`] for malformed or unrelated metadata.
    pub fn validate(&self, chunks: &[SourceChunkV1_3]) -> Result<(), ConditionalActivityError> {
        self.validate_file_iter(chunks.iter().map(|chunk| chunk.source_ref().span().file()))?;
        if let Some(first) = chunks.first() {
            let source = first.source_ref();
            if chunks.iter().any(|chunk| {
                chunk.source_ref().repository() != source.repository()
                    || chunk.source_ref().generation() != source.generation()
            }) {
                return Err(ConditionalActivityError);
            }
        }
        Ok(())
    }

    /// Checks file membership after mapping or response-budget projection.
    ///
    /// # Errors
    /// Returns [`ConditionalActivityError`] for duplicate, missing or extra rows.
    pub fn validate_files(&self, returned: &[FileId]) -> Result<(), ConditionalActivityError> {
        self.validate_file_iter(returned.iter().copied())
    }

    fn validate_file_iter(
        &self,
        returned: impl Iterator<Item = FileId> + Clone,
    ) -> Result<(), ConditionalActivityError> {
        if !self.cause.is_valid()
            || self.files.is_empty()
            || self.files.len() > 32
            || u64::try_from(self.files.len())
                .ok()
                .is_none_or(|files| files > self.cause.members)
            || self
                .files
                .windows(2)
                .any(|pair| pair[0].file >= pair[1].file)
            || self
                .files
                .iter()
                .any(|file| !returned.clone().any(|returned| returned == file.file))
            || returned
                .clone()
                .any(|returned| !self.files.iter().any(|file| file.file == returned))
        {
            return Err(ConditionalActivityError);
        }
        Ok(())
    }
}

/// Source read 1.4 data with unchanged bytes, controls and conditional assessments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceReadDataV1_4 {
    /// Verified chunks in request order.
    #[schemars(length(max = 32))]
    pub chunks: Vec<SourceChunkV1_3>,
    /// Selectors invalid for the pinned source snapshot.
    #[schemars(length(max = 32))]
    pub stale_references: Vec<StaleSourceReference>,
    /// Merged, truncated or unavailable ranges.
    #[schemars(length(max = 64))]
    pub elisions: Vec<SourceElision>,
    /// Exact physical source bytes before JSON escaping.
    #[schemars(range(max = 524_288))]
    pub total_source_bytes: u32,
    /// UTF-8 bytes of Unicode source text; base64 contributes zero.
    #[schemars(range(max = 786_432))]
    pub total_text_bytes: u32,
    /// Bounded source-free plan when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
    /// Ordered source-bound conditional activity assessments.
    #[schemars(length(max = 32))]
    pub conditional_activity: Vec<ConditionalActivityAssessment>,
    /// Present only for genuine aggregate-capacity source recovery.
    /// Absence makes no claim about language support or analysis completeness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_recovery: Option<SourceReadRecovery>,
}

impl SourceReadDataV1_4 {
    /// Checks exact byte totals, conditional evidence and recovery membership.
    ///
    /// # Errors
    /// Returns [`ConditionalActivityError`] for contradictory returned evidence.
    pub fn validate(&self) -> Result<(), ConditionalActivityError> {
        validate_source_text_data(
            &self.chunks,
            &self.conditional_activity,
            self.stale_references.len(),
            self.elisions.len(),
            self.total_source_bytes,
            self.total_text_bytes,
        )?;
        if let Some(recovery) = &self.source_recovery {
            recovery.validate(&self.chunks)?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SourceReadDataV1_4 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            chunks: Vec<SourceChunkV1_3>,
            stale_references: Vec<StaleSourceReference>,
            elisions: Vec<SourceElision>,
            total_source_bytes: u32,
            total_text_bytes: u32,
            #[serde(default)]
            explanation: Option<crate::context::PlanExplanation>,
            conditional_activity: Vec<ConditionalActivityAssessment>,
            #[serde(default)]
            source_recovery: Option<SourceReadRecovery>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            chunks: wire.chunks,
            stale_references: wire.stale_references,
            elisions: wire.elisions,
            total_source_bytes: wire.total_source_bytes,
            total_text_bytes: wire.total_text_bytes,
            explanation: wire.explanation,
            conditional_activity: wire.conditional_activity,
            source_recovery: wire.source_recovery,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// Exact source read success or typed domain error at revision 1.4.
pub type SourceReadOutputV1_4 = EntityToolResponse<
    EntityReadEnvelope<SourceReadDataV1_4, SourceReadRecoveryVersion>,
    SourceReadRecoveryVersion,
>;

#[cfg(test)]
mod tests;
