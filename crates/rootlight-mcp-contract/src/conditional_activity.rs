//! Current-only conditional membership metadata for returned source evidence.
//! Historical locate/source DTOs stay unchanged; these assessments do not prove
//! runtime execution or increase semantic confidence.

use rootlight_ids::FactId;
use rootlight_ir::SourceRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::vertical::{
    LocatedItem, QueryInterpretation, RequiredNullable, SourceChunk, SourceElision,
    StaleSourceReference, ToolSuggestion,
};

/// Stored compilation membership of one exact returned source range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalActivityState {
    /// The admitted inventory proves active membership.
    Active,
    /// The admitted inventory proves inactive membership.
    Inactive,
    /// Available evidence cannot prove either membership state.
    Unproven,
}

/// Source range selected for the conditional membership assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalActivityBasis {
    /// The unique source-backed definition token of a returned symbol.
    DefinitionToken,
    /// The actual returned file or expanded source chunk range.
    ReturnedRange,
}

/// Source-free explanation of an unproven membership assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalActivityReason {
    /// No owning compilation inventory exists.
    NoInventory,
    /// The owning branch inventory is incomplete.
    IncompleteInventory,
    /// No unique source-backed definition exists.
    MissingDefinition,
    /// Competing source-backed definitions exist.
    AmbiguousDefinition,
    /// The returned range crosses distinct compilation scopes.
    CrossScope,
    /// The assessed range is empty.
    EmptyRange,
    /// A complete inventory does not establish membership of the range.
    MembershipUnproven,
    /// A correctness-bearing scan exhausted its admitted budget.
    ScanIncomplete,
    /// The negotiated daemon transport cannot carry the assessment.
    TransportUnavailable,
}

/// Inventory interval inheriting exact file identity from the assessed source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionalActivityScope {
    /// Inclusive byte offset of the persisted inventory scope.
    pub start_byte: u64,
    /// Exclusive byte offset of the persisted inventory scope.
    pub end_byte: u64,
}

/// Exact persisted inventory and parser provenance selected for an assessment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionalActivityInventory {
    /// Typed identity of the selected compilation envelope.
    pub fact_id: FactId,
    /// Typed parser provenance identity carried by that envelope.
    pub parser_provenance_id: FactId,
    /// Owning interval in the assessed file, not another source identity.
    pub scope: ConditionalActivityScope,
    /// Whether the branch inventory is complete, not whole-program validity.
    pub complete: bool,
}

/// Correctness-bearing conditional metadata for one returned item ordinal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionalActivityAssessment {
    /// Zero-based ordinal in the returned matches or chunks.
    pub result_index: u32,
    /// Exact range-selection basis.
    pub basis: ConditionalActivityBasis,
    /// Membership established by stored admitted evidence.
    pub activity: ConditionalActivityState,
    /// Exact token or returned range; null when no unique range was proven.
    pub assessed_source_ref: RequiredNullable<SourceRef>,
    /// Owning persisted inventory; null when none can safely be selected.
    pub inventory: RequiredNullable<ConditionalActivityInventory>,
    /// Unproven reason, required-nullable rather than a wire default.
    pub reason: RequiredNullable<ConditionalActivityReason>,
}

impl ConditionalActivityAssessment {
    /// Checks state, nullable evidence and inventory containment invariants.
    ///
    /// # Errors
    /// Returns [`ConditionalActivityError`] for contradictory or missing evidence.
    pub fn validate(&self) -> Result<(), ConditionalActivityError> {
        let source = self.assessed_source_ref.0.as_ref();
        let inventory = self.inventory.0.as_ref();
        if let Some(inventory) = inventory {
            let source = source.ok_or(ConditionalActivityError)?;
            let span = source.span();
            if inventory.scope.start_byte >= inventory.scope.end_byte
                || span.start_byte() >= span.end_byte()
                || inventory.scope.start_byte > span.start_byte()
                || inventory.scope.end_byte < span.end_byte()
            {
                return Err(ConditionalActivityError);
            }
        }
        match self.activity {
            ConditionalActivityState::Active | ConditionalActivityState::Inactive => {
                if source
                    .is_none_or(|source| source.span().start_byte() >= source.span().end_byte())
                    || inventory.is_none_or(|inventory| !inventory.complete)
                    || self.reason.0.is_some()
                {
                    return Err(ConditionalActivityError);
                }
            }
            ConditionalActivityState::Unproven => {
                match self.reason.0.ok_or(ConditionalActivityError)? {
                    ConditionalActivityReason::IncompleteInventory => {
                        if inventory.is_none_or(|inventory| inventory.complete) {
                            return Err(ConditionalActivityError);
                        }
                    }
                    ConditionalActivityReason::MembershipUnproven => {
                        if inventory.is_none_or(|inventory| !inventory.complete) {
                            return Err(ConditionalActivityError);
                        }
                    }
                    ConditionalActivityReason::MissingDefinition
                    | ConditionalActivityReason::AmbiguousDefinition => {
                        if source.is_some()
                            || inventory.is_some()
                            || self.basis != ConditionalActivityBasis::DefinitionToken
                        {
                            return Err(ConditionalActivityError);
                        }
                    }
                    ConditionalActivityReason::EmptyRange => {
                        if inventory.is_some()
                            || source.is_none_or(|source| {
                                source.span().start_byte() != source.span().end_byte()
                            })
                        {
                            return Err(ConditionalActivityError);
                        }
                    }
                    ConditionalActivityReason::NoInventory
                    | ConditionalActivityReason::CrossScope => {
                        if inventory.is_some()
                            || source.is_none_or(|source| {
                                source.span().start_byte() >= source.span().end_byte()
                            })
                        {
                            return Err(ConditionalActivityError);
                        }
                    }
                    ConditionalActivityReason::ScanIncomplete
                    | ConditionalActivityReason::TransportUnavailable => {
                        if inventory.is_some() {
                            return Err(ConditionalActivityError);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Checks that an assessment belongs to its returned item and expected basis.
    ///
    /// # Errors
    /// Returns [`ConditionalActivityError`] for a foreign or uncontained range.
    pub fn validate_returned_source(
        &self,
        returned: Option<&SourceRef>,
        basis: ConditionalActivityBasis,
    ) -> Result<(), ConditionalActivityError> {
        self.validate()?;
        if self.basis != basis {
            return Err(ConditionalActivityError);
        }
        if basis == ConditionalActivityBasis::ReturnedRange
            && returned.is_some()
            && self.assessed_source_ref.0.is_none()
        {
            return Err(ConditionalActivityError);
        }
        if let Some(assessed) = &self.assessed_source_ref.0 {
            let returned = returned.ok_or(ConditionalActivityError)?;
            if assessed.repository() != returned.repository()
                || assessed.generation() != returned.generation()
                || assessed.span().file() != returned.span().file()
                || assessed.content_hash() != returned.content_hash()
                || assessed.span().start_byte() < returned.span().start_byte()
                || assessed.span().end_byte() > returned.span().end_byte()
                || basis == ConditionalActivityBasis::ReturnedRange
                    && assessed.span() != returned.span()
            {
                return Err(ConditionalActivityError);
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ConditionalActivityAssessment {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            result_index: u32,
            basis: ConditionalActivityBasis,
            activity: ConditionalActivityState,
            #[serde(deserialize_with = "deserialize_nullable")]
            assessed_source_ref: RequiredNullable<SourceRef>,
            #[serde(deserialize_with = "deserialize_nullable")]
            inventory: RequiredNullable<ConditionalActivityInventory>,
            #[serde(deserialize_with = "deserialize_nullable")]
            reason: RequiredNullable<ConditionalActivityReason>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            result_index: wire.result_index,
            basis: wire.basis,
            activity: wire.activity,
            assessed_source_ref: wire.assessed_source_ref,
            inventory: wire.inventory,
            reason: wire.reason,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// Invalid conditional metadata shape or returned-source correlation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid conditional activity evidence")]
pub struct ConditionalActivityError;

/// Current `code.locate` data with required conditional source metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeLocateDataV1_10 {
    /// Deterministically ranked returned matches.
    #[schemars(length(max = 200))]
    pub matches: Vec<LocatedItem>,
    /// Deterministic request interpretation.
    pub query_interpretation: QueryInterpretation,
    /// Bounded next-action suggestions.
    #[schemars(length(max = 16))]
    pub suggested_next: Vec<ToolSuggestion>,
    /// Bounded source-free plan when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
    /// One ordered assessment for each applicable returned item.
    #[schemars(length(max = 200))]
    pub conditional_activity: Vec<ConditionalActivityAssessment>,
}

impl CodeLocateDataV1_10 {
    /// Checks ordinals, assessment shape and returned-item source containment.
    /// Language applicability is enforced by the mapper before language is removed.
    ///
    /// # Errors
    /// Returns [`ConditionalActivityError`] for invalid or duplicate metadata.
    pub fn validate_conditional_activity(&self) -> Result<(), ConditionalActivityError> {
        validate_ordinals(&self.conditional_activity, self.matches.len(), 200)?;
        for assessment in &self.conditional_activity {
            let item = self
                .matches
                .get(
                    usize::try_from(assessment.result_index)
                        .map_err(|_| ConditionalActivityError)?,
                )
                .ok_or(ConditionalActivityError)?;
            let basis = if item.symbol_id.is_some() {
                ConditionalActivityBasis::DefinitionToken
            } else {
                ConditionalActivityBasis::ReturnedRange
            };
            assessment.validate_returned_source(item.source_ref.as_ref(), basis)?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CodeLocateDataV1_10 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            matches: Vec<LocatedItem>,
            query_interpretation: QueryInterpretation,
            suggested_next: Vec<ToolSuggestion>,
            #[serde(default)]
            explanation: Option<crate::context::PlanExplanation>,
            conditional_activity: Vec<ConditionalActivityAssessment>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            matches: wire.matches,
            query_interpretation: wire.query_interpretation,
            suggested_next: wire.suggested_next,
            explanation: wire.explanation,
            conditional_activity: wire.conditional_activity,
        };
        value
            .validate_conditional_activity()
            .map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// Current `source.read` data with required conditional source metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceReadDataV1_2 {
    /// Verified chunks in request order.
    #[schemars(length(max = 32))]
    pub chunks: Vec<SourceChunk>,
    /// Selectors invalid for the pinned source snapshot.
    #[schemars(length(max = 32))]
    pub stale_references: Vec<StaleSourceReference>,
    /// Merged, truncated or unavailable ranges.
    #[schemars(length(max = 64))]
    pub elisions: Vec<SourceElision>,
    /// Exact raw source bytes returned before JSON escaping.
    #[schemars(range(max = 524_288))]
    pub total_source_bytes: u32,
    /// Bounded source-free plan when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
    /// One ordered assessment for each applicable expanded chunk.
    #[schemars(length(max = 32))]
    pub conditional_activity: Vec<ConditionalActivityAssessment>,
}

impl SourceReadDataV1_2 {
    /// Checks exact byte totals, ordinals, applicability and returned chunk ranges.
    ///
    /// # Errors
    /// Returns [`ConditionalActivityError`] for missing or contradictory metadata.
    pub fn validate_conditional_activity(&self) -> Result<(), ConditionalActivityError> {
        validate_ordinals(&self.conditional_activity, self.chunks.len(), 32)?;
        let mut assessments = self.conditional_activity.iter().peekable();
        let mut bytes = 0_u64;
        for (index, chunk) in self.chunks.iter().enumerate() {
            bytes = bytes
                .checked_add(
                    chunk
                        .represented_source_bytes()
                        .map_err(|_| ConditionalActivityError)?,
                )
                .ok_or(ConditionalActivityError)?;
            let assessment = assessments
                .peek()
                .filter(|assessment| usize::try_from(assessment.result_index).ok() == Some(index));
            if is_conditional_activity_language(&chunk.language) != assessment.is_some() {
                return Err(ConditionalActivityError);
            }
            if assessment.is_some() {
                assessments
                    .next()
                    .ok_or(ConditionalActivityError)?
                    .validate_returned_source(
                        Some(&chunk.source_ref),
                        ConditionalActivityBasis::ReturnedRange,
                    )?;
            }
        }
        if bytes != u64::from(self.total_source_bytes) || bytes > 524_288 {
            return Err(ConditionalActivityError);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SourceReadDataV1_2 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            chunks: Vec<SourceChunk>,
            stale_references: Vec<StaleSourceReference>,
            elisions: Vec<SourceElision>,
            total_source_bytes: u32,
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
            explanation: wire.explanation,
            conditional_activity: wire.conditional_activity,
        };
        value
            .validate_conditional_activity()
            .map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// Reports whether the authoritative language uses persisted compilation activity.
#[must_use]
pub fn is_conditional_activity_language(language: &str) -> bool {
    matches!(language, "vba" | "visual-basic-classic")
}

pub(crate) fn validate_ordinals(
    assessments: &[ConditionalActivityAssessment],
    returned: usize,
    maximum: usize,
) -> Result<(), ConditionalActivityError> {
    if returned > maximum
        || assessments.len() > maximum
        || assessments
            .windows(2)
            .any(|pair| pair[0].result_index >= pair[1].result_index)
        || assessments.iter().any(|assessment| {
            usize::try_from(assessment.result_index).map_or(true, |index| index >= returned)
        })
    {
        return Err(ConditionalActivityError);
    }
    Ok(())
}

fn deserialize_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<RequiredNullable<T>, D::Error> {
    Option::<T>::deserialize(deserializer).map(RequiredNullable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        TrustClassification,
        vertical::{EntityKind, LocateReason, SearchMode, SourceEncoding},
    };
    use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId, SymbolId};
    use rootlight_ir::SourceSpan;
    use serde_json::{Value, json};
    use std::collections::BTreeSet;

    fn source(start: u64, end: u64) -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(FileId::from_bytes([3; 20]), start, end).unwrap(),
            ContentHash::from_bytes([4; 32]),
            None,
        )
    }

    fn assessment() -> ConditionalActivityAssessment {
        ConditionalActivityAssessment {
            result_index: 0,
            basis: ConditionalActivityBasis::ReturnedRange,
            activity: ConditionalActivityState::Active,
            assessed_source_ref: RequiredNullable(Some(source(10, 14))),
            inventory: RequiredNullable(Some(ConditionalActivityInventory {
                fact_id: FactId::from_bytes([5; 20]),
                parser_provenance_id: FactId::from_bytes([6; 20]),
                scope: ConditionalActivityScope {
                    start_byte: 0,
                    end_byte: 40,
                },
                complete: true,
            })),
            reason: RequiredNullable(None),
        }
    }

    fn read() -> SourceReadDataV1_2 {
        SourceReadDataV1_2 {
            chunks: vec![SourceChunk {
                extracted_from: None,
                source_ref: source(10, 14),
                path: "Module.bas".into(),
                start_byte: 10,
                end_byte: 14,
                start_line: None,
                end_line: None,
                content: "data".into(),
                encoding: SourceEncoding::Utf8,
                content_hash: ContentHash::from_bytes([4; 32]),
                language: "vba".into(),
                generated: false,
                trust: TrustClassification::UntrustedRepositoryData,
            }],
            stale_references: vec![],
            elisions: vec![],
            total_source_bytes: 4,
            explanation: None,
            conditional_activity: vec![assessment()],
        }
    }

    fn locate() -> CodeLocateDataV1_10 {
        let mut assessment = assessment();
        assessment.basis = ConditionalActivityBasis::DefinitionToken;
        CodeLocateDataV1_10 {
            matches: vec![LocatedItem {
                extracted_from: None,
                symbol_id: Some(SymbolId::from_bytes([7; 20])),
                file_id: Some(FileId::from_bytes([3; 20])),
                kind: EntityKind::Function,
                display_name: "Run".into(),
                signature: None,
                path: "Module.bas".into(),
                score: 1000,
                why: vec![LocateReason::Identifier],
                source_ref: Some(source(0, 40)),
                trust: TrustClassification::UntrustedRepositoryData,
            }],
            query_interpretation: QueryInterpretation {
                tokens: vec!["Run".into()],
                modes: BTreeSet::from([SearchMode::Exact]),
                semantic_available: false,
            },
            suggested_next: vec![],
            explanation: None,
            conditional_activity: vec![assessment],
        }
    }

    #[test]
    fn current_data_requires_sidecar_and_explicit_nullable_fields() {
        for mut value in [
            serde_json::to_value(locate()).unwrap(),
            serde_json::to_value(read()).unwrap(),
        ] {
            let nullable = &value["conditional_activity"][0];
            assert_eq!(nullable["reason"], Value::Null);
            for field in ["assessed_source_ref", "inventory", "reason"] {
                let mut missing = nullable.clone();
                missing.as_object_mut().unwrap().remove(field);
                assert!(
                    serde_json::from_value::<ConditionalActivityAssessment>(missing).is_err(),
                    "{field}"
                );
            }
            value
                .as_object_mut()
                .unwrap()
                .remove("conditional_activity");
            assert!(serde_json::from_value::<CodeLocateDataV1_10>(value.clone()).is_err());
            assert!(serde_json::from_value::<SourceReadDataV1_2>(value).is_err());
        }
    }

    #[test]
    fn state_reason_and_inventory_cannot_contradict_each_other() {
        let valid = serde_json::to_value(assessment()).unwrap();
        for (path, bad) in [
            ("activity", json!("unproven")),
            ("reason", json!("no_inventory")),
            ("assessed_source_ref", Value::Null),
            ("inventory", Value::Null),
        ] {
            let mut value = valid.clone();
            value[path] = bad;
            assert!(serde_json::from_value::<ConditionalActivityAssessment>(value).is_err());
        }
        let mut incomplete = assessment();
        incomplete.activity = ConditionalActivityState::Unproven;
        incomplete.reason.0 = Some(ConditionalActivityReason::IncompleteInventory);
        incomplete.inventory.0.as_mut().unwrap().complete = false;
        assert!(incomplete.validate().is_ok());
        incomplete.reason.0 = Some(ConditionalActivityReason::MembershipUnproven);
        assert!(incomplete.validate().is_err());
        incomplete.inventory.0.as_mut().unwrap().complete = true;
        assert!(incomplete.validate().is_ok());
        incomplete.inventory.0.as_mut().unwrap().scope.start_byte = 12;
        assert!(incomplete.validate().is_err());
        for reason in [
            ConditionalActivityReason::NoInventory,
            ConditionalActivityReason::CrossScope,
        ] {
            let mut absent = assessment();
            absent.basis = ConditionalActivityBasis::DefinitionToken;
            absent.activity = ConditionalActivityState::Unproven;
            absent.inventory.0 = None;
            absent.reason.0 = Some(reason);
            assert!(absent.validate().is_ok());
            for source in [None, Some(source(10, 10))] {
                absent.assessed_source_ref.0 = source;
                assert!(absent.validate().is_err(), "{reason:?}");
                assert!(
                    serde_json::from_value::<ConditionalActivityAssessment>(
                        serde_json::to_value(&absent).unwrap()
                    )
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn locate_rejects_duplicate_out_of_range_and_foreign_source_metadata() {
        let valid = locate();
        assert_eq!(
            serde_json::from_value::<CodeLocateDataV1_10>(serde_json::to_value(&valid).unwrap())
                .unwrap(),
            valid
        );
        let mut duplicate = valid.clone();
        duplicate
            .conditional_activity
            .push(duplicate.conditional_activity[0].clone());
        assert!(duplicate.validate_conditional_activity().is_err());
        let mut invalid = valid.clone();
        invalid.conditional_activity[0].result_index = 1;
        assert!(invalid.validate_conditional_activity().is_err());
        let mut invalid = valid.clone();
        invalid.conditional_activity[0].assessed_source_ref.0 = Some(source(39, 41));
        invalid.conditional_activity[0]
            .inventory
            .0
            .as_mut()
            .unwrap()
            .scope
            .end_byte = 50;
        assert!(invalid.validate_conditional_activity().is_err());
        let mut foreign = serde_json::to_value(valid).unwrap();
        foreign["conditional_activity"][0]["assessed_source_ref"]["span"]["file"] =
            serde_json::to_value(FileId::from_bytes([8; 20])).unwrap();
        assert!(serde_json::from_value::<CodeLocateDataV1_10>(foreign).is_err());
    }

    #[test]
    fn missing_or_ambiguous_definition_never_selects_an_arbitrary_token() {
        for reason in [
            ConditionalActivityReason::MissingDefinition,
            ConditionalActivityReason::AmbiguousDefinition,
        ] {
            let mut value = locate();
            let assessment = &mut value.conditional_activity[0];
            assessment.activity = ConditionalActivityState::Unproven;
            assessment.reason.0 = Some(reason);
            assessment.inventory.0 = None;
            assert!(value.validate_conditional_activity().is_err());
            value.conditional_activity[0].assessed_source_ref.0 = None;
            assert!(value.validate_conditional_activity().is_ok());
        }
    }

    #[test]
    fn source_read_requires_applicable_actual_ranges_and_exact_byte_totals() {
        let valid = read();
        assert_eq!(
            serde_json::from_value::<SourceReadDataV1_2>(serde_json::to_value(&valid).unwrap())
                .unwrap(),
            valid
        );
        let mut invalid = valid.clone();
        invalid.conditional_activity.clear();
        assert!(invalid.validate_conditional_activity().is_err());
        let mut invalid = valid.clone();
        invalid.chunks[0].language = "rust".into();
        assert!(invalid.validate_conditional_activity().is_err());
        invalid.conditional_activity.clear();
        assert!(invalid.validate_conditional_activity().is_ok());
        let mut invalid = valid.clone();
        invalid.total_source_bytes = 3;
        assert!(invalid.validate_conditional_activity().is_err());
        let mut invalid = valid;
        invalid.conditional_activity[0].assessed_source_ref.0 = Some(source(11, 13));
        assert!(invalid.validate_conditional_activity().is_err());
        invalid.conditional_activity[0].activity = ConditionalActivityState::Unproven;
        invalid.conditional_activity[0].reason.0 = Some(ConditionalActivityReason::ScanIncomplete);
        invalid.conditional_activity[0].inventory.0 = None;
        invalid.conditional_activity[0].assessed_source_ref.0 = None;
        assert!(invalid.validate_conditional_activity().is_err());
    }

    #[test]
    fn historical_data_does_not_accept_the_new_sidecar() {
        assert!(
            serde_json::from_value::<crate::vertical::CodeLocateData>(
                serde_json::to_value(locate()).unwrap()
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<crate::vertical::SourceReadData>(
                serde_json::to_value(read()).unwrap()
            )
            .is_err()
        );
    }
}
