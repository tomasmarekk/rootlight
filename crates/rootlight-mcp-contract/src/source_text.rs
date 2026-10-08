//! Versioned, byte-identifiable source text without a duplicate raw payload.

use std::fmt;

use rootlight_ids::ContentHash;
use rootlight_ir::SourceRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::TrustClassification;
use crate::conditional_activity::{
    ConditionalActivityAssessment, ConditionalActivityBasis, ConditionalActivityError,
    is_conditional_activity_language, validate_ordinals,
};
use crate::source_entity::{EntityReadEnvelope, EntityToolResponse};
use crate::vertical::{
    GenerationSelector, RepositorySelector, ResponseBudget, ResponseProfile, SourceChunk,
    SourceElision, SourceEncoding, SourceEncodingRequest, SourceReadInput, SourceReadSelector,
    StaleSourceReference, extracted_origin_matches_source,
};

/// Contract revision with lossless physical-encoding-aware text.
pub const SOURCE_READ_TEXT_VERSION: &str = "1.3";

/// Exact source-text response revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SourceReadTextVersion {
    /// Source read contract 1.3.
    #[serde(rename = "1.3")]
    V1_3,
}

/// Requested representation in source read 1.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceTextEncodingRequest {
    /// Strict text preserving original scalar spelling and physical byte identity.
    LosslessText,
    /// Historical exact UTF-8 mode.
    Utf8LosslessWhenValid,
    /// Historical exact-byte base64 mode.
    BytesBase64,
}

/// Strict source read 1.3 input; omitted encoding selects lossless text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceReadInputV1_3 {
    /// Owning repository.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Exact source selectors.
    #[schemars(length(min = 1, max = 32))]
    pub references: Vec<SourceReadSelector>,
    /// Leading context lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 50))]
    pub context_lines_before: Option<u8>,
    /// Trailing context lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 50))]
    pub context_lines_after: Option<u8>,
    /// Merge overlapping verified ranges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_overlaps: Option<bool>,
    /// Aggregate physical source-byte ceiling.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 524_288))]
    pub max_source_bytes: Option<u32>,
    /// Include one-based line numbers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_line_numbers: Option<bool>,
    /// Representation; omission means lossless text in this revision only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<SourceTextEncodingRequest>,
    /// Optional lower response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested representation profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
    /// Return the bounded source-free plan without retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

impl SourceReadInputV1_3 {
    /// Returns this revision's defaulted representation.
    #[must_use]
    pub fn effective_encoding(&self) -> SourceTextEncodingRequest {
        self.encoding
            .unwrap_or(SourceTextEncodingRequest::LosslessText)
    }

    /// Converts shared retrieval controls without changing historical types.
    ///
    /// For lossless text, the returned encoding is a control-only placeholder;
    /// callers must use the dedicated lossless port, not the legacy reader.
    #[must_use]
    pub fn into_legacy_controls(self) -> SourceReadInput {
        let encoding = self.effective_encoding();
        SourceReadInput {
            repository: self.repository,
            generation: self.generation,
            references: self.references,
            context_lines_before: self.context_lines_before,
            context_lines_after: self.context_lines_after,
            merge_overlaps: self.merge_overlaps,
            max_source_bytes: self.max_source_bytes,
            include_line_numbers: self.include_line_numbers,
            encoding: Some(match encoding {
                SourceTextEncodingRequest::LosslessText
                | SourceTextEncodingRequest::Utf8LosslessWhenValid => {
                    SourceEncodingRequest::Utf8LosslessWhenValid
                }
                SourceTextEncodingRequest::BytesBase64 => SourceEncodingRequest::BytesBase64,
            }),
            budget: self.budget,
            response_profile: self.response_profile,
            explain: self.explain,
        }
    }
}

/// Physical encoding of original source bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SourceTextEncoding {
    /// Original UTF-8 bytes.
    #[serde(rename = "utf-8")]
    Utf8,
    /// BOM-marked UTF-16 little endian.
    #[serde(rename = "utf-16le")]
    Utf16Le,
    /// BOM-marked UTF-16 big endian.
    #[serde(rename = "utf-16be")]
    Utf16Be,
}

/// Disjoint discriminator for the single-content lossless branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum LosslessContentEncoding {
    /// Unicode presentation reconstructible using the physical encoding.
    #[serde(rename = "lossless_text")]
    LosslessText,
}

/// One exact scalar-preserving text chunk with physical source coordinates.
#[derive(Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceTextChunk {
    /// Immediate physical container of an extracted source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_from: Option<SourceRef>,
    /// Generation-bound original-byte reference.
    pub source_ref: SourceRef,
    /// Repository-relative display path.
    #[schemars(length(min = 1, max = 8192))]
    pub path: String,
    /// Inclusive original byte start.
    pub start_byte: u64,
    /// Exclusive original byte end.
    pub end_byte: u64,
    /// First included one-based line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u64>,
    /// Last included one-based line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u64>,
    /// Exact Unicode presentation, including a selected original BOM.
    /// UTF-16 can expand at most three UTF-8 bytes per two physical bytes.
    #[schemars(length(max = 786_432))]
    pub content: String,
    /// Closed representation discriminator.
    pub encoding: LosslessContentEncoding,
    /// Encoding needed to reconstruct the selected original bytes.
    pub physical_encoding: SourceTextEncoding,
    /// Complete original-file hash, not a hash of the text presentation.
    pub content_hash: ContentHash,
    /// Indexed language identity.
    #[schemars(length(min = 1, max = 256))]
    pub language: String,
    /// Whether the indexed file is generated.
    pub generated: bool,
    /// Repository-data trust marker.
    pub trust: TrustClassification,
}

impl fmt::Debug for SourceTextChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceTextChunk")
            .field("source_ref", &self.source_ref)
            .field("physical_encoding", &self.physical_encoding)
            .field("text_bytes", &self.content.len())
            .finish_non_exhaustive()
    }
}

impl SourceTextChunk {
    /// Checks byte reconstruction width and bound source metadata without allocation.
    ///
    /// # Errors
    /// Returns [`ConditionalActivityError`] for contradictory physical claims.
    pub fn validate(&self) -> Result<(), ConditionalActivityError> {
        let bytes = match self.physical_encoding {
            SourceTextEncoding::Utf8 => u64::try_from(self.content.len()).ok(),
            SourceTextEncoding::Utf16Le | SourceTextEncoding::Utf16Be => {
                if !self.start_byte.is_multiple_of(2)
                    || !self.end_byte.is_multiple_of(2)
                    || self.content.contains('\0')
                    || (self.start_byte == 0
                        && !self.content.is_empty()
                        && !self.content.starts_with('\u{feff}'))
                {
                    return Err(ConditionalActivityError);
                }
                self.content.chars().try_fold(0_u64, |total, character| {
                    total.checked_add(u64::try_from(character.len_utf16()).ok()?.checked_mul(2)?)
                })
            }
        }
        .ok_or(ConditionalActivityError)?;
        let span = self.source_ref.span();
        let lines_valid = match (self.start_line, self.end_line) {
            (None, None) => true,
            (Some(start), Some(end)) => start > 0 && start <= end,
            _ => false,
        };
        if !lines_valid
            || bytes > 524_288
            || self.content.len() > 786_432
            || self.path.is_empty()
            || self.path.len() > 8192
            || self.language.is_empty()
            || self.language.len() > 256
            || self.end_byte.checked_sub(self.start_byte) != Some(bytes)
            || span.start_byte() != self.start_byte
            || span.end_byte() != self.end_byte
            || self.source_ref.content_hash() != self.content_hash
            || self
                .extracted_from
                .as_ref()
                .is_some_and(|origin| !extracted_origin_matches_source(origin, &self.source_ref))
            || self.source_ref.line_hint().is_some_and(|hint| {
                self.start_line != Some(hint.start_line()) || self.end_line != Some(hint.end_line())
            })
        {
            return Err(ConditionalActivityError);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SourceTextChunk {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            extracted_from: Option<SourceRef>,
            source_ref: SourceRef,
            path: String,
            start_byte: u64,
            end_byte: u64,
            start_line: Option<u64>,
            end_line: Option<u64>,
            content: String,
            encoding: LosslessContentEncoding,
            physical_encoding: SourceTextEncoding,
            content_hash: ContentHash,
            language: String,
            generated: bool,
            trust: TrustClassification,
        }
        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            extracted_from: wire.extracted_from,
            source_ref: wire.source_ref,
            path: wire.path,
            start_byte: wire.start_byte,
            end_byte: wire.end_byte,
            start_line: wire.start_line,
            end_line: wire.end_line,
            content: wire.content,
            encoding: wire.encoding,
            physical_encoding: wire.physical_encoding,
            content_hash: wire.content_hash,
            language: wire.language,
            generated: wire.generated,
            trust: wire.trust,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// Disjoint legacy or lossless chunk shapes; encoding selects exactly one branch.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SourceChunkV1_3 {
    /// A single Unicode presentation with original physical encoding.
    Lossless(SourceTextChunk),
    /// An unchanged legacy UTF-8 or base64 representation.
    Legacy(SourceChunk),
}

impl fmt::Debug for SourceChunkV1_3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceChunkV1_3")
            .field("source_ref", self.source_ref())
            .field("byte_totals", &self.byte_totals())
            .finish_non_exhaustive()
    }
}

impl SourceChunkV1_3 {
    /// Returns the exact original-byte reference.
    #[must_use]
    pub fn source_ref(&self) -> &SourceRef {
        match self {
            Self::Lossless(chunk) => &chunk.source_ref,
            Self::Legacy(chunk) => &chunk.source_ref,
        }
    }
    /// Returns the indexed language identity.
    #[must_use]
    pub fn language(&self) -> &str {
        match self {
            Self::Lossless(chunk) => &chunk.language,
            Self::Legacy(chunk) => &chunk.language,
        }
    }
    /// Returns the mandatory repository-data trust marker.
    #[must_use]
    pub fn trust(&self) -> TrustClassification {
        match self {
            Self::Lossless(chunk) => chunk.trust,
            Self::Legacy(chunk) => chunk.trust,
        }
    }
    /// Returns physical and Unicode-presentation byte counts.
    ///
    /// # Errors
    /// Returns [`ConditionalActivityError`] for invalid physical representation.
    pub fn byte_totals(&self) -> Result<(u64, u64), ConditionalActivityError> {
        match self {
            Self::Lossless(chunk) => {
                chunk.validate()?;
                Ok((
                    chunk.end_byte - chunk.start_byte,
                    u64::try_from(chunk.content.len()).map_err(|_| ConditionalActivityError)?,
                ))
            }
            Self::Legacy(chunk) => Ok((
                chunk
                    .represented_source_bytes()
                    .map_err(|_| ConditionalActivityError)?,
                if chunk.encoding == SourceEncoding::Utf8 {
                    u64::try_from(chunk.content.len()).map_err(|_| ConditionalActivityError)?
                } else {
                    0
                },
            )),
        }
    }
}

/// Source read 1.3 data with independent physical and presentation totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceReadDataV1_3 {
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
}

impl SourceReadDataV1_3 {
    /// Checks independent totals and exact conditional activity binding.
    ///
    /// # Errors
    /// Returns [`ConditionalActivityError`] for contradictory returned evidence.
    pub fn validate_conditional_activity(&self) -> Result<(), ConditionalActivityError> {
        validate_ordinals(&self.conditional_activity, self.chunks.len(), 32)?;
        if self.stale_references.len() > 32 || self.elisions.len() > 64 {
            return Err(ConditionalActivityError);
        }
        let mut assessments = self.conditional_activity.iter().peekable();
        let mut totals = (0_u64, 0_u64);
        for (index, chunk) in self.chunks.iter().enumerate() {
            let (physical, text) = chunk.byte_totals()?;
            totals.0 = totals
                .0
                .checked_add(physical)
                .ok_or(ConditionalActivityError)?;
            totals.1 = totals.1.checked_add(text).ok_or(ConditionalActivityError)?;
            let assessment = assessments
                .peek()
                .filter(|assessment| usize::try_from(assessment.result_index).ok() == Some(index));
            if is_conditional_activity_language(chunk.language()) != assessment.is_some() {
                return Err(ConditionalActivityError);
            }
            if assessment.is_some() {
                assessments
                    .next()
                    .ok_or(ConditionalActivityError)?
                    .validate_returned_source(
                        Some(chunk.source_ref()),
                        ConditionalActivityBasis::ReturnedRange,
                    )?;
            }
        }
        if totals
            != (
                u64::from(self.total_source_bytes),
                u64::from(self.total_text_bytes),
            )
            || totals.0 > 524_288
            || totals.1 > 786_432
        {
            return Err(ConditionalActivityError);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SourceReadDataV1_3 {
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
        };
        value
            .validate_conditional_activity()
            .map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// Exact lossless-text source read success or typed domain error.
pub type SourceReadOutputV1_3 = EntityToolResponse<
    EntityReadEnvelope<SourceReadDataV1_3, SourceReadTextVersion>,
    SourceReadTextVersion,
>;

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, GenerationId, RepositoryId};
    use rootlight_ir::SourceSpan;
    use serde_json::{Value, json};

    fn chunk(content: &str, encoding: SourceTextEncoding, start: u64) -> SourceTextChunk {
        let width = if encoding == SourceTextEncoding::Utf8 {
            content.len() as u64
        } else {
            content.encode_utf16().count() as u64 * 2
        };
        let hash = ContentHash::from_bytes([4; 32]);
        SourceTextChunk {
            extracted_from: None,
            source_ref: SourceRef::new(
                RepositoryId::from_bytes([1; 16]),
                GenerationId::from_bytes([2; 20]),
                SourceSpan::new(FileId::from_bytes([3; 20]), start, start + width).unwrap(),
                hash,
                None,
            ),
            path: "src/module.rs".into(),
            start_byte: start,
            end_byte: start + width,
            start_line: None,
            end_line: None,
            content: content.into(),
            encoding: LosslessContentEncoding::LosslessText,
            physical_encoding: encoding,
            content_hash: hash,
            language: "rust".into(),
            generated: false,
            trust: TrustClassification::UntrustedRepositoryData,
        }
    }

    #[test]
    fn lossless_chunks_preserve_bom_crlf_non_bmp_and_distinct_totals() {
        for encoding in [
            SourceTextEncoding::Utf8,
            SourceTextEncoding::Utf16Le,
            SourceTextEncoding::Utf16Be,
        ] {
            let chunk = chunk("\u{feff}a\r\n\u{010a}\u{1f642}", encoding, 0);
            let value = SourceChunkV1_3::Lossless(chunk.clone());
            let (physical, text) = value.byte_totals().unwrap();
            let data = SourceReadDataV1_3 {
                chunks: vec![value],
                stale_references: Vec::new(),
                elisions: Vec::new(),
                total_source_bytes: u32::try_from(physical)
                    .expect("bounded fixture physical bytes fit the wire counter"),
                total_text_bytes: u32::try_from(text)
                    .expect("bounded fixture text bytes fit the wire counter"),
                explanation: None,
                conditional_activity: Vec::new(),
            };
            let wire = serde_json::to_value(&data).unwrap();
            assert_eq!(wire["chunks"][0]["encoding"], "lossless_text");
            assert!(wire["chunks"][0].get("exact_content").is_none());
            assert_eq!(
                serde_json::from_value::<SourceReadDataV1_3>(wire).unwrap(),
                data
            );
            assert_eq!(text, chunk.content.len() as u64);
            if encoding != SourceTextEncoding::Utf8 {
                assert_ne!(physical, text);
            }
            assert!(!format!("{chunk:?}").contains(&chunk.content));
        }
    }

    #[test]
    fn lossless_chunk_contract_rejects_width_alignment_marker_and_metadata_lies() {
        let valid =
            serde_json::to_value(chunk("\u{feff}x", SourceTextEncoding::Utf16Le, 0)).unwrap();
        for (key, replacement) in [
            ("physical_encoding", json!("utf16le")),
            ("encoding", json!("utf8")),
            ("content", json!("xy")),
            ("content", json!("\u{feff}\0")),
            ("end_byte", json!(5)),
            ("start_line", json!(0)),
            ("extra", json!(true)),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = replacement;
            assert!(
                serde_json::from_value::<SourceChunkV1_3>(invalid).is_err(),
                "{key}"
            );
        }
        assert!(
            chunk("x", SourceTextEncoding::Utf16Le, 3)
                .validate()
                .is_err()
        );
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            assert!(chunk("", encoding, 0).validate().is_ok());
            assert!(chunk("\u{feff}", encoding, 0).validate().is_ok());
            assert!(chunk("\u{1f642}", encoding, 2).validate().is_ok());
        }
        assert!(chunk("\0", SourceTextEncoding::Utf8, 0).validate().is_ok());
    }

    #[test]
    fn source_read_text_data_rejects_each_independent_total_and_legacy_base64_is_not_text() {
        let chunk = chunk("\u{feff}\u{010a}", SourceTextEncoding::Utf16Be, 0);
        let valid = SourceReadDataV1_3 {
            chunks: vec![SourceChunkV1_3::Lossless(chunk)],
            stale_references: Vec::new(),
            elisions: Vec::new(),
            total_source_bytes: 4,
            total_text_bytes: 5,
            explanation: None,
            conditional_activity: Vec::new(),
        };
        let wire = serde_json::to_value(&valid).unwrap();
        for key in ["total_source_bytes", "total_text_bytes"] {
            let mut invalid = wire.clone();
            invalid[key] = json!(3);
            assert!(serde_json::from_value::<SourceReadDataV1_3>(invalid).is_err());
        }
        let legacy = SourceChunk {
            extracted_from: None,
            source_ref: valid.chunks[0].source_ref().clone(),
            path: "source.bin".into(),
            start_byte: 0,
            end_byte: 4,
            start_line: None,
            end_line: None,
            content: "/v8KAQ==".into(),
            encoding: SourceEncoding::Base64,
            content_hash: ContentHash::from_bytes([4; 32]),
            language: "rust".into(),
            generated: false,
            trust: TrustClassification::UntrustedRepositoryData,
        };
        let wire = serde_json::to_value(&legacy).unwrap();
        let current = serde_json::from_value::<SourceChunkV1_3>(wire.clone()).unwrap();
        assert_eq!(current.byte_totals().unwrap(), (4, 0));
        assert_eq!(serde_json::to_value(current).unwrap(), wire);
        let mut mixed = wire;
        mixed["physical_encoding"] = json!("utf-16be");
        assert!(serde_json::from_value::<SourceChunkV1_3>(mixed).is_err());
    }

    #[test]
    fn only_new_input_defaults_to_lossless_and_historical_input_remains_closed() {
        let source = chunk("x", SourceTextEncoding::Utf8, 0).source_ref;
        let mut wire = json!({"repository": {"repository_id": source.repository()}, "references": [{"source_ref": source}]});
        let current = serde_json::from_value::<SourceReadInputV1_3>(wire.clone()).unwrap();
        assert_eq!(
            current.effective_encoding(),
            SourceTextEncodingRequest::LosslessText
        );
        let historical = serde_json::from_value::<SourceReadInput>(wire.clone()).unwrap();
        assert_eq!(historical.encoding, None);
        wire["encoding"] = json!("lossless_text");
        assert!(serde_json::from_value::<SourceReadInput>(wire.clone()).is_err());
        assert!(serde_json::from_value::<SourceReadInputV1_3>(wire.clone()).is_ok());
        wire["encoding"] = Value::String("lossy_text".into());
        assert!(serde_json::from_value::<SourceReadInputV1_3>(wire).is_err());
    }
}
