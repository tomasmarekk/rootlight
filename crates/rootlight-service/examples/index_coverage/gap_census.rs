//! Summarizes retained skipped evidence without copying source bodies or full keys.
//! Syntax correlations are a separate exact-source view, not semantic gap owners.
//! Diagnostic retention limits never alter indexed facts or production budgets.

use std::collections::BTreeMap;

use rootlight_ir::{
    CoverageStatus, DiagnosticRecord, DiagnosticSeverity, FactDomain, OccurrenceRecord,
    SkippedRegion, SkippedRegionReason,
};
use serde::Serialize;
use serde_json::{Value, json};

const MAX_GROUPS: usize = 512;
const MAX_CORRELATION_GROUPS: usize = 512;
const MAX_KEY_BYTES: usize = 4096;
const MAX_PREVIEW_BYTES: usize = 128;
const MAX_MARKER_SAMPLES: usize = 3;

type GapKey<'a> = (&'a str, FactDomain, SkippedRegionReason, &'a str);
type CorrelationKey<'a> = (GapKey<'a>, &'a str);

#[derive(Debug, thiserror::Error)]
pub(super) enum CensusError {
    #[error("gap census accounting overflow")]
    Overflow,
    #[error("gap census does not conserve retained records")]
    Conservation,
}

#[derive(Clone, Copy)]
struct Limits {
    groups: usize,
    correlation_groups: usize,
    key_bytes: usize,
    preview_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            groups: MAX_GROUPS,
            correlation_groups: MAX_CORRELATION_GROUPS,
            key_bytes: MAX_KEY_BYTES,
            preview_bytes: MAX_PREVIEW_BYTES,
        }
    }
}

#[derive(Default, Serialize)]
struct Counts {
    total_records: u64,
    grouped_records: u64,
    ungrouped_capacity_records: u64,
    ungrouped_oversize_key_records: u64,
}

impl Counts {
    fn validate(&self, mut grouped: impl Iterator<Item = u64>) -> Result<(), CensusError> {
        let sum = grouped.try_fold(0_u64, |sum, count| {
            sum.checked_add(count).ok_or(CensusError::Overflow)
        })?;
        let total = self
            .grouped_records
            .checked_add(self.ungrouped_capacity_records)
            .and_then(|value| value.checked_add(self.ungrouped_oversize_key_records))
            .ok_or(CensusError::Overflow)?;
        if sum != self.grouped_records || total != self.total_records {
            return Err(CensusError::Conservation);
        }
        Ok(())
    }
}

fn increment(value: &mut u64) -> Result<(), CensusError> {
    *value = value.checked_add(1).ok_or(CensusError::Overflow)?;
    Ok(())
}

fn record_key<K: Copy + Ord>(
    groups: &mut BTreeMap<K, u64>,
    counts: &mut Counts,
    key: K,
    key_bytes: usize,
    maximum_groups: usize,
    maximum_key_bytes: usize,
) -> Result<(), CensusError> {
    increment(&mut counts.total_records)?;
    if key_bytes > maximum_key_bytes {
        return increment(&mut counts.ungrouped_oversize_key_records);
    }
    if let Some(count) = groups.get_mut(&key) {
        increment(count)?;
        return increment(&mut counts.grouped_records);
    }
    if maximum_groups == 0 {
        return increment(&mut counts.ungrouped_capacity_records);
    }
    if groups.len() == maximum_groups {
        if groups
            .last_key_value()
            .is_some_and(|(last, _)| key >= *last)
        {
            return increment(&mut counts.ungrouped_capacity_records);
        }
        // The largest retained key only decreases, so an evicted key can never
        // be readmitted later with a partial count. Selection is order-neutral.
        let (_, evicted) = groups.pop_last().ok_or(CensusError::Conservation)?;
        counts.grouped_records = counts
            .grouped_records
            .checked_sub(evicted)
            .ok_or(CensusError::Conservation)?;
        counts.ungrouped_capacity_records = counts
            .ungrouped_capacity_records
            .checked_add(evicted)
            .ok_or(CensusError::Overflow)?;
    }
    groups.insert(key, 1);
    increment(&mut counts.grouped_records)
}

fn preview(value: &str, maximum_bytes: usize) -> Value {
    let mut end = value.len().min(maximum_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    json!({
        "preview": &value[..end],
        "full_bytes": value.len(),
        "full_content_hash": rootlight_ids::content_hash(value.as_bytes()),
        "truncated": end != value.len(),
    })
}

fn key_fields(key: GapKey<'_>, maximum_preview_bytes: usize) -> Value {
    json!({
        "language": preview(key.0, maximum_preview_bytes),
        "domain": key.1,
        "reason": key.2,
        "detail": preview(key.3, maximum_preview_bytes),
    })
}

/// Counts actual retained gaps and already indexed per-file occurrence candidates.
///
/// Full keys stay borrowed from the identity-verified snapshot. Capacity selects
/// the lexicographically smallest full keys, independent of observation order.
pub(super) struct GapCensus<'a> {
    limits: Limits,
    groups: BTreeMap<GapKey<'a>, u64>,
    correlations: BTreeMap<CorrelationKey<'a>, u64>,
    gaps: Counts,
    matches: Counts,
    matched_gaps: u64,
    unmatched_gaps: u64,
    multiple_match_gaps: u64,
}

impl<'a> GapCensus<'a> {
    /// Creates a bounded diagnostic collector without retaining source bytes.
    pub(super) fn new() -> Self {
        Self::with_limits(Limits::default())
    }

    fn with_limits(limits: Limits) -> Self {
        Self {
            limits,
            groups: BTreeMap::new(),
            correlations: BTreeMap::new(),
            gaps: Counts::default(),
            matches: Counts::default(),
            matched_gaps: 0,
            unmatched_gaps: 0,
            multiple_match_gaps: 0,
        }
    }

    /// Admits one gap and its existing per-file, same-span candidate slice.
    ///
    /// # Errors
    /// Returns an accounting error on overflow or broken internal conservation.
    pub(super) fn record(
        &mut self,
        language: &'a str,
        gap: &'a SkippedRegion,
        candidates: &[&'a OccurrenceRecord],
    ) -> Result<(), CensusError> {
        let key = (language, gap.domain, gap.reason, gap.detail.as_str());
        let key_bytes = language
            .len()
            .checked_add(gap.detail.len())
            .ok_or(CensusError::Overflow)?;
        record_key(
            &mut self.groups,
            &mut self.gaps,
            key,
            key_bytes,
            self.limits.groups,
            self.limits.key_bytes,
        )?;
        let mut matched = 0_u64;
        for site in candidates {
            if !super::occurrence_matches_gap(site, gap) {
                continue;
            }
            increment(&mut matched)?;
            let correlation_bytes = key_bytes
                .checked_add(site.syntax_kind.len())
                .ok_or(CensusError::Overflow)?;
            record_key(
                &mut self.correlations,
                &mut self.matches,
                (key, site.syntax_kind.as_str()),
                correlation_bytes,
                self.limits.correlation_groups,
                self.limits.key_bytes,
            )?;
        }
        if matched == 0 {
            increment(&mut self.unmatched_gaps)?;
        } else {
            increment(&mut self.matched_gaps)?;
            if matched > 1 {
                increment(&mut self.multiple_match_gaps)?;
            }
        }
        Ok(())
    }

    /// Produces finite metadata-only views and generation-global omission evidence.
    ///
    /// # Errors
    /// Rejects overflow, missing gap admissions, or inconsistent retained counts.
    pub(super) fn finish(
        self,
        diagnostics: &[DiagnosticRecord],
        expected_gaps: usize,
    ) -> Result<Value, CensusError> {
        self.gaps.validate(self.groups.values().copied())?;
        self.matches.validate(self.correlations.values().copied())?;
        if self.gaps.total_records
            != u64::try_from(expected_gaps).map_err(|_| CensusError::Overflow)?
            || self.matched_gaps.checked_add(self.unmatched_gaps) != Some(self.gaps.total_records)
        {
            return Err(CensusError::Conservation);
        }
        let groups: Vec<_> = self
            .groups
            .iter()
            .map(|(key, count)| {
                let mut value = key_fields(*key, self.limits.preview_bytes);
                value["gap_records"] = json!(count);
                value
            })
            .collect();
        let correlations: Vec<_> = self
            .correlations
            .iter()
            .map(|((key, syntax), count)| {
                let mut value = key_fields(*key, self.limits.preview_bytes);
                value["syntax_kind"] = preview(syntax, self.limits.preview_bytes);
                value["occurrence_matches"] = json!(count);
                value
            })
            .collect();
        let (omissions, aggregate_truncation_observed) = omission_evidence(diagnostics)?;
        Ok(json!({
            "schema": "rootlight-gap-census/1",
            "input_scope": "retained_identity_verified_skipped_regions",
            "metadata_is_untrusted_data": true,
            "selection": "lexicographically_smallest_full_keys",
            "key_size_basis": "variable_utf8_language_detail_and_optional_syntax_kind",
            "limits": {
                "maximum_groups": self.limits.groups,
                "maximum_correlation_groups": self.limits.correlation_groups,
                "maximum_full_key_bytes": self.limits.key_bytes,
                "maximum_preview_bytes": self.limits.preview_bytes,
                "maximum_omission_samples_per_code": MAX_MARKER_SAMPLES,
            },
            "gaps": self.gaps,
            "groups": groups,
            "correlation": {
                "meaning": "exact_source_matches_not_semantic_gap_owners",
                "matched_gap_records": self.matched_gaps,
                "unmatched_gap_records": self.unmatched_gaps,
                "multiple_match_gap_records": self.multiple_match_gaps,
                "matches": self.matches,
                "groups": correlations,
            },
            "global_omission_evidence": omissions,
            "aggregate_truncation_observed": aggregate_truncation_observed,
            "omitted_detail_attribution": "unavailable",
            "coverage_interpretation": "unknown",
            "full_semantic_completeness_proven": false,
            "installed_mcp_qualified": false,
        }))
    }
}

const OMISSION_CODES: [(&str, Option<&str>); 5] = [
    (
        "skipped-region-details-bounded",
        Some(" skipped-region detail records omitted by aggregate resource limit"),
    ),
    (
        "extension-coverage-bounded",
        Some(" optional lexical extensions omitted by aggregate resource limit"),
    ),
    (
        "aggregate-diagnostics-truncated",
        Some(" source diagnostics omitted by aggregate resource limit"),
    ),
    ("project-adapter-diagnostics-truncated", None),
    ("project-adapter-facts-truncated", None),
];

fn formatted_count(message: &str, suffix: &str) -> Option<u64> {
    let number = message.strip_suffix(suffix)?;
    if number.is_empty()
        || number.starts_with('0')
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    number.parse().ok()
}

fn omission_evidence(diagnostics: &[DiagnosticRecord]) -> Result<(Vec<Value>, bool), CensusError> {
    let mut output = Vec::with_capacity(OMISSION_CODES.len());
    let mut any_marker = false;
    for (code, suffix) in OMISSION_CODES {
        let mut seen = 0_u64;
        let mut malformed = 0_u64;
        let mut source_scoped = 0_u64;
        let mut count = None;
        let mut samples = Vec::new();
        for diagnostic in diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == code)
        {
            any_marker = true;
            increment(&mut seen)?;
            if diagnostic.source.is_some() {
                increment(&mut source_scoped)?;
            }
            let parsed = suffix.and_then(|suffix| formatted_count(&diagnostic.message, suffix));
            if suffix.is_some() && parsed.is_none() {
                increment(&mut malformed)?;
            }
            if seen == 1
                && diagnostic.source.is_none()
                && diagnostic.severity == DiagnosticSeverity::Warning
                && diagnostic.coverage_effect == CoverageStatus::Bounded
            {
                count = parsed;
            } else {
                count = None;
            }
            if samples.len() < MAX_MARKER_SAMPLES {
                samples.push(json!({
                    "id": diagnostic.id,
                    "source_scoped": diagnostic.source.is_some(),
                    "message": preview(&diagnostic.message, MAX_PREVIEW_BYTES),
                }));
            }
        }
        output.push(json!({
            "code": code,
            "scope": "generation_global_no_language_attribution",
            "markers_seen": seen,
            "source_scoped_markers": source_scoped,
            "malformed_numeric_markers": malformed,
            "reported_omitted_count": count,
            "count_status": if seen == 0 { "not_reported" } else if count.is_some() {
                "exact_formatter_derived_reported_count"
            } else { "unknown" },
            "samples": samples,
            "samples_truncated": seen > u64::try_from(MAX_MARKER_SAMPLES).map_err(|_| CensusError::Overflow)?,
        }));
    }
    Ok((output, any_marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId};
    use rootlight_ir::{
        Confidence, FactEvidence, OccurrenceRole, OccurrenceTarget, SourceRef, SourceSpan,
    };

    fn source() -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(FileId::from_bytes([3; 20]), 4, 12).unwrap(),
            rootlight_ids::content_hash(b"source"),
            None,
        )
    }

    fn gap(detail: &str) -> SkippedRegion {
        let source = source();
        SkippedRegion {
            id: FactId::from_bytes([4; 20]),
            repository: source.repository(),
            generation: source.generation(),
            source: source.clone(),
            domain: FactDomain::Entities,
            reason: SkippedRegionReason::UnsupportedConstruct,
            detail: detail.to_owned(),
            provenance: FactId::from_bytes([5; 20]),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        }
    }

    fn occurrence(kind: &str) -> OccurrenceRecord {
        let source = source();
        OccurrenceRecord {
            id: FactId::from_bytes([6; 20]),
            repository: source.repository(),
            generation: source.generation(),
            file: source.span().file(),
            source: source.clone(),
            role: OccurrenceRole::CallSite,
            enclosing: None,
            target: OccurrenceTarget::Unresolved {
                text_hash: rootlight_ids::content_hash(b"name"),
            },
            syntactic_text_hash: rootlight_ids::content_hash(b"name"),
            syntax_kind: kind.to_owned(),
            provenance: FactId::from_bytes([5; 20]),
            confidence: Confidence::new(900).unwrap(),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        }
    }

    fn diagnostic(code: &str, message: &str) -> DiagnosticRecord {
        let source = source();
        DiagnosticRecord {
            id: FactId::from_bytes([7; 20]),
            repository: source.repository(),
            generation: source.generation(),
            code: code.to_owned(),
            message: message.to_owned(),
            severity: DiagnosticSeverity::Warning,
            source: None,
            coverage_effect: CoverageStatus::Bounded,
            provenance: FactId::from_bytes([5; 20]),
            evidence: FactEvidence {
                source: None,
                derivation: Vec::new(),
            },
        }
    }

    fn limits(groups: usize, key_bytes: usize, preview_bytes: usize) -> Limits {
        Limits {
            groups,
            correlation_groups: groups,
            key_bytes,
            preview_bytes,
        }
    }

    #[test]
    fn full_detail_variants_never_merge_equal_previews() {
        let first = gap("same-prefix-left");
        let second = gap("same-prefix-right");
        let mut census = GapCensus::with_limits(limits(4, 64, 11));
        census.record("fsharp", &first, &[]).unwrap();
        census.record("fsharp", &first, &[]).unwrap();
        census.record("fsharp", &second, &[]).unwrap();
        let output = census.finish(&[], 3).unwrap();
        let groups = output["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0]["detail"]["preview"],
            groups[1]["detail"]["preview"]
        );
        assert_ne!(
            groups[0]["detail"]["full_content_hash"],
            groups[1]["detail"]["full_content_hash"]
        );
        assert_eq!(groups[0]["gap_records"], 2);
        assert_eq!(groups[1]["gap_records"], 1);
        assert!(
            groups
                .iter()
                .all(|group| group["detail"]["truncated"] == true)
        );
        assert_eq!(output["gaps"]["total_records"], 3);
        assert_eq!(output["gaps"]["grouped_records"], 3);
        assert_eq!(output["coverage_interpretation"], "unknown");
        assert_eq!(output["full_semantic_completeness_proven"], false);
        assert_eq!(output["installed_mcp_qualified"], false);
    }

    #[test]
    fn language_domain_and_reason_are_independent_full_key_dimensions() {
        let first = gap("same");
        let mut relation = gap("same");
        relation.domain = FactDomain::Relations;
        let mut parse = gap("same");
        parse.reason = SkippedRegionReason::ParseError;
        let mut census = GapCensus::new();
        census.record("first", &first, &[]).unwrap();
        census.record("second", &first, &[]).unwrap();
        census.record("first", &relation, &[]).unwrap();
        census.record("first", &parse, &[]).unwrap();
        let output = census.finish(&[], 4).unwrap();
        assert_eq!(output["groups"].as_array().unwrap().len(), 4);
        assert_eq!(output["gaps"]["grouped_records"], 4);
    }

    #[test]
    fn two_same_span_labels_count_one_gap_and_two_correlations() {
        let gap = gap("target-unavailable");
        let first = occurrence("neutral.application");
        let second = occurrence("neutral.identifier");
        let mut foreign = occurrence("neutral.foreign");
        foreign.source = SourceRef::new(
            first.source.repository(),
            first.source.generation(),
            first.source.span(),
            rootlight_ids::content_hash(b"other content"),
            None,
        );
        let mut census = GapCensus::new();
        census
            .record("fsharp", &gap, &[&first, &second, &foreign])
            .unwrap();
        let output = census.finish(&[], 1).unwrap();
        assert_eq!(output["gaps"]["total_records"], 1);
        assert_eq!(output["correlation"]["matches"]["total_records"], 2);
        assert_eq!(output["correlation"]["groups"].as_array().unwrap().len(), 2);
        assert_eq!(output["correlation"]["matched_gap_records"], 1);
        assert_eq!(output["correlation"]["multiple_match_gap_records"], 1);
        assert_eq!(output["correlation"]["unmatched_gap_records"], 0);
        assert_eq!(
            output["correlation"]["meaning"],
            "exact_source_matches_not_semantic_gap_owners"
        );
    }

    #[test]
    fn capacity_eviction_and_oversize_keys_conserve_all_records() {
        let records = [
            gap("z"),
            gap("z"),
            gap("a"),
            gap("b"),
            gap("a"),
            gap("too-long"),
        ];
        let mut census = GapCensus::with_limits(limits(2, 7, 1));
        for record in &records {
            census.record("fsharp", record, &[]).unwrap();
        }
        let output = census.finish(&[], records.len()).unwrap();
        assert_eq!(output["gaps"]["total_records"], 6);
        assert_eq!(output["gaps"]["grouped_records"], 3);
        assert_eq!(output["gaps"]["ungrouped_capacity_records"], 2);
        assert_eq!(output["gaps"]["ungrouped_oversize_key_records"], 1);
        assert_eq!(output["groups"][0]["detail"]["preview"], "a");
        assert_eq!(output["groups"][1]["detail"]["preview"], "b");
    }

    #[test]
    fn correlation_capacity_and_oversize_keys_are_independent_of_base_counts() {
        let gap = gap("a");
        let first = occurrence("x");
        let second = occurrence("y");
        let oversized = occurrence("oversized-kind");
        let mut census = GapCensus::with_limits(limits(1, 8, 1));
        census
            .record("fsharp", &gap, &[&first, &second, &oversized])
            .unwrap();
        let output = census.finish(&[], 1).unwrap();
        assert_eq!(output["gaps"]["grouped_records"], 1);
        assert_eq!(output["correlation"]["matches"]["total_records"], 3);
        assert_eq!(output["correlation"]["matches"]["grouped_records"], 1);
        assert_eq!(
            output["correlation"]["matches"]["ungrouped_capacity_records"],
            1
        );
        assert_eq!(
            output["correlation"]["matches"]["ungrouped_oversize_key_records"],
            1
        );
    }

    #[test]
    fn bounded_selection_and_counts_are_permutation_invariant() {
        let records = [gap("z"), gap("b"), gap("b"), gap("a"), gap("z"), gap("c")];
        let mut forward = GapCensus::with_limits(limits(2, 64, 8));
        let mut reverse = GapCensus::with_limits(limits(2, 64, 8));
        for record in &records {
            forward.record("fsharp", record, &[]).unwrap();
        }
        for record in records.iter().rev() {
            reverse.record("fsharp", record, &[]).unwrap();
        }
        assert_eq!(
            forward.finish(&[], records.len()).unwrap(),
            reverse.finish(&[], records.len()).unwrap()
        );
    }

    #[test]
    fn previews_preserve_scalar_boundaries_and_full_metadata_hashes() {
        let value = "ab☃𝄞suffix";
        let output = preview(value, 4);
        assert_eq!(output["preview"], "ab");
        assert_eq!(output["full_bytes"], value.len());
        assert_eq!(
            output["full_content_hash"],
            serde_json::to_value(rootlight_ids::content_hash(value.as_bytes())).unwrap()
        );
        assert_eq!(output["truncated"], true);
        assert_eq!(preview(value, value.len())["truncated"], false);
        assert_eq!(preview(value, 0)["preview"], "");
    }

    #[test]
    fn exact_omission_markers_are_global_and_do_not_enter_gap_counts() {
        let diagnostics = [
            diagnostic(
                "skipped-region-details-bounded",
                "17 skipped-region detail records omitted by aggregate resource limit",
            ),
            diagnostic(
                "extension-coverage-bounded",
                "19 optional lexical extensions omitted by aggregate resource limit",
            ),
            diagnostic(
                "aggregate-diagnostics-truncated",
                "23 source diagnostics omitted by aggregate resource limit",
            ),
        ];
        let output = GapCensus::new().finish(&diagnostics, 0).unwrap();
        assert_eq!(output["gaps"]["total_records"], 0);
        assert_eq!(output["aggregate_truncation_observed"], true);
        assert_eq!(output["coverage_interpretation"], "unknown");
        for (marker, expected) in output["global_omission_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .zip([17, 19, 23])
        {
            assert_eq!(marker["reported_omitted_count"], expected);
            assert_eq!(marker["scope"], "generation_global_no_language_attribution");
            assert!(marker.get("language").is_none());
        }
    }

    #[test]
    fn duplicate_source_scoped_or_unformatted_omissions_remain_unknown() {
        let mut scoped = diagnostic(
            "extension-coverage-bounded",
            "2 optional lexical extensions omitted by aggregate resource limit",
        );
        scoped.source = Some(source());
        let diagnostics = [
            diagnostic(
                "skipped-region-details-bounded",
                "1 skipped-region detail records omitted by aggregate resource limit",
            ),
            diagnostic(
                "skipped-region-details-bounded",
                "2 skipped-region detail records omitted by aggregate resource limit",
            ),
            scoped,
            diagnostic("aggregate-diagnostics-truncated", "unknown count"),
            diagnostic(
                "project-adapter-facts-truncated",
                "additional project semantic facts were omitted by aggregate resource limits",
            ),
        ];
        let output = GapCensus::new().finish(&diagnostics, 0).unwrap();
        let markers = output["global_omission_evidence"].as_array().unwrap();
        for index in [0, 1, 2, 4] {
            assert_eq!(markers[index]["count_status"], "unknown");
            assert!(markers[index]["reported_omitted_count"].is_null());
        }
        assert_eq!(markers[0]["markers_seen"], 2);
        assert_eq!(markers[1]["source_scoped_markers"], 1);
        assert_eq!(markers[2]["malformed_numeric_markers"], 1);
        assert_eq!(markers[3]["count_status"], "not_reported");
        assert!(markers[3]["reported_omitted_count"].is_null());
    }

    #[test]
    fn formatter_counts_reject_repair_overflow_and_message_suffix_changes() {
        let suffix = " skipped-region detail records omitted by aggregate resource limit";
        for prefix in [
            "",
            "0",
            "01",
            "+1",
            "-1",
            " 1",
            "1 ",
            "18446744073709551616",
        ] {
            assert_eq!(formatted_count(&format!("{prefix}{suffix}"), suffix), None);
        }
        assert_eq!(formatted_count(&format!("17{suffix}"), suffix), Some(17));
        assert_eq!(formatted_count(&format!("17{suffix}."), suffix), None);
    }

    #[test]
    fn omission_samples_are_bounded_without_losing_marker_counts() {
        let diagnostics: Vec<_> = (1_u8..=5)
            .map(|count| {
                let mut marker = diagnostic(
                    "skipped-region-details-bounded",
                    &format!(
                        "{count} skipped-region detail records omitted by aggregate resource limit"
                    ),
                );
                marker.id = FactId::from_bytes([count; 20]);
                marker
            })
            .collect();
        let output = GapCensus::new().finish(&diagnostics, 0).unwrap();
        let marker = &output["global_omission_evidence"][0];
        assert_eq!(marker["markers_seen"], 5);
        assert_eq!(
            marker["samples"].as_array().unwrap().len(),
            MAX_MARKER_SAMPLES
        );
        assert_eq!(marker["samples_truncated"], true);
        assert_eq!(marker["count_status"], "unknown");
        assert!(marker["reported_omitted_count"].is_null());
    }

    #[test]
    fn zero_capacity_and_missing_admissions_are_explicit() {
        let gap = gap("a");
        let mut census = GapCensus::with_limits(limits(0, 64, 8));
        census.record("fsharp", &gap, &[]).unwrap();
        let output = census.finish(&[], 1).unwrap();
        assert_eq!(output["gaps"]["ungrouped_capacity_records"], 1);
        assert!(output["groups"].as_array().unwrap().is_empty());
        assert!(matches!(
            GapCensus::new().finish(&[], 1),
            Err(CensusError::Conservation)
        ));
    }

    #[test]
    fn counters_fail_with_a_typed_overflow() {
        let gap = gap("a");
        let mut census = GapCensus::new();
        census.gaps.total_records = u64::MAX;
        assert!(matches!(
            census.record("fsharp", &gap, &[]),
            Err(CensusError::Overflow)
        ));
    }
}
