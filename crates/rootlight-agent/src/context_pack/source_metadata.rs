//! Quote-bound source reservations for already selected context items.
//! Provider costs follow unique references; presentation costs follow items.

use std::{
    io::{self, Write},
    time::Instant,
};

use rootlight_ir::SourceRef;
use serde::Serialize;

use super::{
    CONTEXT_PACK_TIMEOUT_MS, ContextEvidencePlanningError, ContextPackPlanningError,
    JSON_ESCAPE_EXPANSION, MAX_PACK_ITEMS, MIN_SOURCE_MATERIAL_BYTES, SIGNATURE_BYTES,
    SOURCE_LANGUAGE_BYTES, SourceMaterializationPlan, add_budget_charge, budget_charge_fits,
    source_completeness,
};
use crate::{
    context_evidence::{
        CONTEXT_SOURCE_METADATA_QUOTE_POLICY_VERSION, ContextEvidenceCallContext,
        ContextEvidenceCollectionError, ContextEvidencePort, ContextEvidencePortErrorKind,
        ContextSourceMetadataFixedUpper, ContextSourceMetadataQuoteOutput,
        ContextSourceMetadataQuoteRequest,
    },
    policy::{BudgetCharge, BudgetLedger, CancellationSignal},
};

#[derive(Debug)]
pub(super) enum QuoteBoundsOutcome {
    Complete(SourceQuoteBounds),
    Omitted(
        &'static str,
        rootlight_mcp_contract::completeness::ResultCompleteness,
    ),
}

pub(super) async fn quote_bounds<P, C>(
    port: &P,
    request: ContextSourceMetadataQuoteRequest,
    budget: &mut BudgetLedger,
    cancellation: C,
    deadline: Instant,
) -> Result<QuoteBoundsOutcome, ContextEvidencePlanningError>
where
    P: ContextEvidencePort<C>,
    C: CancellationSignal + Clone + Send + Sync + 'static,
{
    if cancellation.is_cancelled() {
        return Err(ContextEvidenceCollectionError::Cancelled.into());
    }
    if Instant::now() >= deadline {
        return Err(ContextEvidenceCollectionError::DeadlineExceeded.into());
    }
    let quote_budget = budget.remaining();
    if [
        quote_budget.rows,
        quote_budget.results,
        quote_budget.tokens,
        quote_budget.source_bytes,
        quote_budget.traversal_facts,
        quote_budget.depth,
        quote_budget.paths,
        quote_budget.json_bytes,
        quote_budget.memory_bytes,
        quote_budget.time_ms,
    ]
    .into_iter()
    .any(|capacity| capacity == 0)
    {
        return Ok(QuoteBoundsOutcome::Omitted(
            "source_budget",
            source_completeness(ContextEvidencePortErrorKind::Unavailable, true)?,
        ));
    }
    let reservation = budget
        .reserve(quote_budget)
        .map_err(ContextPackPlanningError::from)?;
    let response = port
        .quote_source_metadata(
            request.clone(),
            ContextEvidenceCallContext::new(cancellation.clone(), deadline, quote_budget),
        )
        .await;
    let usage = match &response {
        Ok(output) => output.usage,
        Err(error) => error.usage,
    };
    reservation
        .commit(usage)
        .map_err(ContextPackPlanningError::from)?;
    if cancellation.is_cancelled() {
        return Err(ContextEvidenceCollectionError::Cancelled.into());
    }
    if Instant::now() >= deadline {
        return Err(ContextEvidenceCollectionError::DeadlineExceeded.into());
    }
    let output = match response {
        Ok(output) => output,
        Err(error) => {
            let label = match error.kind {
                ContextEvidencePortErrorKind::Cancelled => {
                    return Err(ContextEvidenceCollectionError::Cancelled.into());
                }
                ContextEvidencePortErrorKind::DeadlineExceeded => {
                    return Err(ContextEvidenceCollectionError::DeadlineExceeded.into());
                }
                ContextEvidencePortErrorKind::InvalidResponse => {
                    return Err(ContextEvidenceCollectionError::InvalidProviderResponse.into());
                }
                ContextEvidencePortErrorKind::Unsupported => "source_metadata_unsupported",
                ContextEvidencePortErrorKind::Unavailable => "source_metadata_unavailable",
            };
            return Ok(QuoteBoundsOutcome::Omitted(
                label,
                source_completeness(error.kind, false)?,
            ));
        }
    };
    if output.repository != request.repository
        || output.generation != request.generation
        || output.quote_policy_version != CONTEXT_SOURCE_METADATA_QUOTE_POLICY_VERSION
        || output.usage.source_bytes != 0
    {
        return Err(ContextEvidenceCollectionError::InvalidProviderResponse.into());
    }
    let completeness = checked_completeness(&output.completeness)?;
    if completeness.state != rootlight_mcp_contract::completeness::CompletenessState::Complete {
        return Ok(QuoteBoundsOutcome::Omitted(
            "source_metadata_incomplete",
            completeness,
        ));
    }
    Ok(QuoteBoundsOutcome::Complete(SourceQuoteBounds::new(
        &request, &output,
    )?))
}

#[derive(Debug, Clone, Copy)]
struct PrefixBound {
    unique_references: u64,
    fixed: ContextSourceMetadataFixedUpper,
    shaping_json: u64,
    shaping_memory: u64,
}

#[derive(Debug)]
pub(super) struct SourceQuoteBounds {
    prefixes: Vec<PrefixBound>,
}

impl SourceQuoteBounds {
    pub(super) fn new(
        request: &ContextSourceMetadataQuoteRequest,
        output: &ContextSourceMetadataQuoteOutput,
    ) -> Result<Self, ContextEvidenceCollectionError> {
        if request.targets.is_empty()
            || request.targets.len() > MAX_PACK_ITEMS
            || output.repository != request.repository
            || output.generation != request.generation
            || output.quote_policy_version != CONTEXT_SOURCE_METADATA_QUOTE_POLICY_VERSION
            || output.usage.source_bytes != 0
            || output.completeness
                != rootlight_mcp_contract::completeness::ResultCompleteness::complete()
            || output.completeness.state
                != rootlight_mcp_contract::completeness::CompletenessState::Complete
        {
            return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
        }
        let mut seen = Vec::new();
        let mut prefixes = Vec::new();
        seen.try_reserve_exact(request.targets.len())
            .map_err(|_| ContextEvidenceCollectionError::InvalidProviderResponse)?;
        prefixes
            .try_reserve_exact(request.targets.len())
            .map_err(|_| ContextEvidenceCollectionError::InvalidProviderResponse)?;
        let mut fixed = ContextSourceMetadataFixedUpper::default();
        let mut shaping_json = 0_u64;
        let mut shaping_memory = 0_u64;
        for target in &request.targets {
            let reference = &target.source_ref;
            if reference.repository() != request.repository
                || reference.generation() != request.generation
            {
                return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
            }
            if !seen.contains(&reference) {
                let entry = output
                    .entries
                    .get(seen.len())
                    .ok_or(ContextEvidenceCollectionError::InvalidProviderResponse)?;
                let unique = u64::try_from(seen.len() + 1)
                    .map_err(|_| ContextEvidenceCollectionError::InvalidProviderResponse)?;
                if entry.source_ref != *reference
                    || !valid_fixed(entry.prefix_fixed_upper, unique)
                    || !monotone(fixed, entry.prefix_fixed_upper)
                {
                    return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
                }
                fixed = entry.prefix_fixed_upper;
                seen.push(reference);
            }
            let metadata = shaping_fixed(reference, request.include_snippets)?;
            shaping_json = shaping_json
                .checked_add(metadata.json_bytes)
                .ok_or(ContextEvidenceCollectionError::InvalidProviderResponse)?;
            shaping_memory = shaping_memory
                .checked_add(metadata.memory_bytes)
                .ok_or(ContextEvidenceCollectionError::InvalidProviderResponse)?;
            prefixes.push(PrefixBound {
                unique_references: u64::try_from(seen.len())
                    .map_err(|_| ContextEvidenceCollectionError::InvalidProviderResponse)?,
                fixed,
                shaping_json,
                shaping_memory,
            });
        }
        if output.entries.len() != seen.len()
            || output.usage.results
                != u64::try_from(seen.len())
                    .map_err(|_| ContextEvidenceCollectionError::InvalidProviderResponse)?
        {
            return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
        }
        Ok(Self { prefixes })
    }

    pub(super) fn provider(&self, items: usize, max_bytes: u32) -> Option<BudgetCharge> {
        let prefix = self.prefixes.get(items.checked_sub(1)?)?;
        let text = prefix.unique_references.checked_mul(u64::from(max_bytes))?;
        let physical = text.checked_mul(2)?;
        let json = text
            .checked_mul(JSON_ESCAPE_EXPANSION)?
            .checked_add(prefix.fixed.json_bytes)?;
        Some(BudgetCharge {
            rows: prefix.fixed.rows,
            results: prefix.unique_references,
            tokens: json,
            source_bytes: physical,
            traversal_facts: prefix.fixed.traversal_facts,
            depth: 1,
            paths: prefix.fixed.paths,
            json_bytes: json,
            memory_bytes: prefix
                .fixed
                .memory_bytes
                .checked_add(json)?
                .checked_add(physical)?
                .checked_add(text)?,
            time_ms: u64::from(CONTEXT_PACK_TIMEOUT_MS),
            ..BudgetCharge::default()
        })
    }

    pub(super) fn shaping(
        &self,
        items: usize,
        max_bytes: u32,
        include_snippets: bool,
    ) -> Option<BudgetCharge> {
        let prefix = self.prefixes.get(items.checked_sub(1)?)?;
        let text = if include_snippets {
            u64::from(max_bytes)
        } else {
            u64::from(SIGNATURE_BYTES.min(max_bytes))
        };
        let language = if include_snippets {
            SOURCE_LANGUAGE_BYTES
        } else {
            0
        };
        let items = u64::try_from(items).ok()?;
        // Provider material can still own the validated duplicate signature
        // until shaping returns, even though the response omits that field.
        let temporary_signature = if include_snippets {
            u64::from(SIGNATURE_BYTES.min(max_bytes))
        } else {
            0
        };
        let presentation = items.checked_mul(
            text.checked_add(language)?
                .checked_add(temporary_signature)?,
        )?;
        // Language labels reject control characters; only quotes and
        // backslashes expand in JSON, by at most two bytes per input byte.
        let json = text
            .checked_mul(JSON_ESCAPE_EXPANSION)?
            .checked_add(language.checked_mul(2)?)?
            .checked_mul(items)?
            .checked_add(prefix.shaping_json)?;
        Some(BudgetCharge {
            tokens: json,
            json_bytes: json,
            memory_bytes: presentation
                .checked_mul(2)?
                .checked_add(prefix.shaping_memory)?,
            ..BudgetCharge::default()
        })
    }

    pub(super) fn combined(
        &self,
        items: usize,
        max_bytes: u32,
        include_snippets: bool,
    ) -> Option<BudgetCharge> {
        Some(add_budget_charge(
            self.provider(items, max_bytes)?,
            self.shaping(items, max_bytes, include_snippets)?,
        ))
    }

    pub(super) fn affordable(
        &self,
        remaining: BudgetCharge,
        configured_max_bytes: u32,
        include_snippets: bool,
    ) -> SourceMaterializationPlan {
        let minimum = MIN_SOURCE_MATERIAL_BYTES.min(configured_max_bytes);
        for items in (1..=self.prefixes.len()).rev() {
            if !self
                .combined(items, minimum, include_snippets)
                .is_some_and(|charge| budget_charge_fits(charge, remaining))
            {
                continue;
            }
            let mut lower = minimum;
            let mut upper = configured_max_bytes;
            while lower < upper {
                let candidate = lower + (upper - lower).div_ceil(2);
                if self
                    .combined(items, candidate, include_snippets)
                    .is_some_and(|charge| budget_charge_fits(charge, remaining))
                {
                    lower = candidate;
                } else {
                    upper = candidate - 1;
                }
            }
            return SourceMaterializationPlan {
                target_count: items,
                max_bytes_per_snippet: lower,
            };
        }
        SourceMaterializationPlan {
            target_count: 0,
            max_bytes_per_snippet: 0,
        }
    }
}

fn monotone(
    previous: ContextSourceMetadataFixedUpper,
    next: ContextSourceMetadataFixedUpper,
) -> bool {
    previous.json_bytes <= next.json_bytes
        && previous.memory_bytes <= next.memory_bytes
        && previous.rows <= next.rows
        && previous.traversal_facts <= next.traversal_facts
        && previous.paths <= next.paths
}

fn valid_fixed(fixed: ContextSourceMetadataFixedUpper, unique: u64) -> bool {
    fixed.json_bytes > 0
        && fixed.memory_bytes > 0
        && fixed.rows >= unique
        && fixed.paths >= unique
        && [
            fixed.json_bytes,
            fixed.memory_bytes,
            fixed.rows,
            fixed.traversal_facts,
            fixed.paths,
        ]
        .into_iter()
        .all(|value| usize::try_from(value).is_ok())
}

fn checked_completeness(
    value: &rootlight_mcp_contract::completeness::ResultCompleteness,
) -> Result<rootlight_mcp_contract::completeness::ResultCompleteness, ContextEvidenceCollectionError>
{
    if value.limiting_resources.len() > 14 || value.guidance.len() > 9 {
        return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
    }
    rootlight_mcp_contract::completeness::ResultCompleteness::new(
        value.state,
        value.limiting_resources.clone(),
        value.continuation,
        value.guidance.clone(),
    )
    .map_err(|_| ContextEvidenceCollectionError::InvalidProviderResponse)
}

#[derive(Serialize)]
struct ShapingMetadata<'a> {
    source_ref: &'a SourceRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<&'static str>,
    snippet: Option<rootlight_mcp_contract::context::RepositorySnippet>,
}

pub(super) fn shaping_fixed(
    reference: &SourceRef,
    include_snippets: bool,
) -> Result<BudgetCharge, ContextEvidenceCollectionError> {
    let metadata = ShapingMetadata {
        source_ref: reference,
        signature: (!include_snippets).then_some(""),
        snippet: include_snippets.then(|| rootlight_mcp_contract::context::RepositorySnippet {
            source_ref: reference.clone(),
            content: String::new(),
            language: String::new(),
            provenance: rootlight_mcp_contract::context::SnippetProvenance::SourceRead,
            truncated: false,
            trust: rootlight_mcp_contract::TrustClassification::UntrustedRepositoryData,
        }),
    };
    let mut counter = JsonCounter::default();
    serde_json::to_writer(&mut counter, &metadata)
        .map_err(|_| ContextEvidenceCollectionError::InvalidProviderResponse)?;
    Ok(BudgetCharge {
        tokens: counter.bytes,
        json_bytes: counter.bytes,
        memory_bytes: counter.bytes,
        ..BudgetCharge::default()
    })
}

#[derive(Default)]
struct JsonCounter {
    bytes: u64,
}

impl Write for JsonCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(
                u64::try_from(bytes.len())
                    .map_err(|_| io::Error::other("source metadata width overflow"))?,
            )
            .ok_or_else(|| io::Error::other("source metadata width overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::context_evidence::{ContextSourceMetadataQuoteEntry, ContextSourceTarget};
    use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId};
    use rootlight_ir::{LineRange, SourceSpan};
    use rootlight_mcp_contract::{completeness::ResultCompleteness, context::SourcePolicy};

    pub(in crate::context_pack) fn fixture_request(
        items: usize,
        snippets: bool,
    ) -> ContextSourceMetadataQuoteRequest {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        ContextSourceMetadataQuoteRequest {
            repository,
            generation,
            source_policy: SourcePolicy::FocusedSnippets,
            include_snippets: snippets,
            targets: (0..items)
                .map(|index| ContextSourceTarget {
                    candidate_id: crate::context_evidence::EvidenceCandidateId::fixture(format!(
                        "quoted-item-{index}"
                    )),
                    source_ref: SourceRef::new(
                        repository,
                        generation,
                        SourceSpan::new(
                            FileId::from_bytes([u8::try_from(index).unwrap(); 20]),
                            0,
                            8192,
                        )
                        .unwrap(),
                        ContentHash::from_bytes([3; 32]),
                        Some(LineRange::new(1, 256).unwrap()),
                    ),
                })
                .collect(),
        }
    }

    pub(in crate::context_pack) fn fixture_quote(
        request: &ContextSourceMetadataQuoteRequest,
    ) -> ContextSourceMetadataQuoteOutput {
        let mut entries = Vec::new();
        for target in &request.targets {
            if entries
                .iter()
                .any(|entry: &ContextSourceMetadataQuoteEntry| {
                    entry.source_ref == target.source_ref
                })
            {
                continue;
            }
            let count = u64::try_from(entries.len() + 1).unwrap();
            entries.push(ContextSourceMetadataQuoteEntry {
                source_ref: target.source_ref.clone(),
                prefix_fixed_upper: ContextSourceMetadataFixedUpper {
                    json_bytes: count * 2048,
                    memory_bytes: count * 2048,
                    rows: count,
                    traversal_facts: count * 10,
                    paths: count,
                },
            });
        }
        let results = u64::try_from(entries.len()).unwrap();
        ContextSourceMetadataQuoteOutput {
            repository: request.repository,
            generation: request.generation,
            quote_policy_version: CONTEXT_SOURCE_METADATA_QUOTE_POLICY_VERSION,
            entries,
            completeness: ResultCompleteness::complete(),
            usage: BudgetCharge {
                results,
                tokens: 1,
                ..BudgetCharge::default()
            },
        }
    }

    pub(in crate::context_pack) fn fixture_bounds(
        items: usize,
        snippets: bool,
    ) -> SourceQuoteBounds {
        let request = fixture_request(items, snippets);
        SourceQuoteBounds::new(&request, &fixture_quote(&request)).unwrap()
    }

    #[test]
    fn source_metadata_long_escaped_fixed_quote_shrinks_before_omitting_items() {
        for path in ["p".repeat(4096), "\"\\\u{1}".repeat(1400)] {
            let request = fixture_request(2, true);
            let mut output = fixture_quote(&request);
            let serialized = serde_json::to_vec(&serde_json::json!({
                "path": path, "language": "neutral", "source_ref": request.targets[0].source_ref,
            }))
            .unwrap();
            let fixed = u64::try_from(serialized.len()).unwrap();
            for (index, entry) in output.entries.iter_mut().enumerate() {
                let count = u64::try_from(index + 1).unwrap();
                entry.prefix_fixed_upper.json_bytes = fixed * count;
                entry.prefix_fixed_upper.memory_bytes = fixed * count;
            }
            let bounds = SourceQuoteBounds::new(&request, &output).unwrap();
            let exact = bounds.combined(2, 73, true).unwrap();
            assert_eq!(
                bounds.affordable(exact, 8192, true),
                SourceMaterializationPlan {
                    target_count: 2,
                    max_bytes_per_snippet: 73,
                }
            );
            let one_under = BudgetCharge {
                tokens: exact.tokens - 1,
                ..exact
            };
            assert_eq!(
                bounds.affordable(one_under, 8192, true),
                SourceMaterializationPlan {
                    target_count: 2,
                    max_bytes_per_snippet: 72,
                }
            );
            let minimum = bounds.combined(1, 64, true).unwrap();
            assert_eq!(bounds.affordable(minimum, 8192, true).target_count, 1);
            assert_eq!(
                bounds
                    .affordable(
                        BudgetCharge {
                            json_bytes: minimum.json_bytes - 1,
                            ..minimum
                        },
                        8192,
                        true
                    )
                    .target_count,
                0
            );
            assert_eq!(bounds.provider(1, 64).unwrap().json_bytes, fixed + 6 * 64);
        }
    }

    #[test]
    fn source_metadata_duplicate_full_refs_fetch_once_but_shape_each_item() {
        let mut request = fixture_request(2, true);
        request.targets[1].source_ref = request.targets[0].source_ref.clone();
        let output = fixture_quote(&request);
        assert_eq!(output.entries.len(), 1);
        let bounds = SourceQuoteBounds::new(&request, &output).unwrap();
        assert_eq!(bounds.provider(1, 64), bounds.provider(2, 64));
        let one = bounds.shaping(1, 64, true).unwrap();
        let two = bounds.shaping(2, 64, true).unwrap();
        assert_eq!(two.json_bytes, 2 * one.json_bytes);
        assert_eq!(two.memory_bytes, 2 * one.memory_bytes);
        assert_eq!(bounds.provider(2, 64).unwrap().results, 1);
        assert_eq!(request.targets[0].source_ref, output.entries[0].source_ref);
        assert_ne!(
            request.targets[0].candidate_id,
            request.targets[1].candidate_id
        );
    }

    #[test]
    fn source_metadata_full_hash_and_line_hint_are_not_dedup_shortcuts() {
        let mut request = fixture_request(3, false);
        let original = request.targets[0].source_ref.clone();
        request.targets[1].source_ref = SourceRef::new(
            request.repository,
            request.generation,
            original.span(),
            ContentHash::from_bytes([9; 32]),
            original.line_hint(),
        );
        request.targets[2].source_ref = SourceRef::new(
            request.repository,
            request.generation,
            original.span(),
            original.content_hash(),
            Some(LineRange::new(2, 257).unwrap()),
        );
        let output = fixture_quote(&request);
        assert_eq!(output.entries.len(), 3);
        assert_eq!(
            SourceQuoteBounds::new(&request, &output)
                .unwrap()
                .provider(3, 64)
                .unwrap()
                .results,
            3
        );
    }

    #[test]
    fn source_metadata_cumulative_batch_prefix_is_never_averaged() {
        let request = fixture_request(33, true);
        let mut output = fixture_quote(&request);
        for (index, entry) in output.entries.iter_mut().enumerate() {
            let count = u64::try_from(index + 1).unwrap();
            let batch_fixed = if count <= 32 { 1000 } else { 9000 };
            entry.prefix_fixed_upper.json_bytes = batch_fixed + 77 * count;
            entry.prefix_fixed_upper.memory_bytes = batch_fixed + 99 * count;
        }
        let bounds = SourceQuoteBounds::new(&request, &output).unwrap();
        assert_eq!(
            bounds.provider(1, 64).unwrap().json_bytes,
            1000 + 77 + 6 * 64
        );
        assert_eq!(
            bounds.provider(32, 64).unwrap().json_bytes,
            1000 + 77 * 32 + 6 * 64 * 32
        );
        assert_eq!(
            bounds.provider(33, 64).unwrap().json_bytes,
            9000 + 77 * 33 + 6 * 64 * 33
        );
        let only_first = bounds.combined(1, 64, true).unwrap();
        assert_eq!(bounds.affordable(only_first, 8192, true).target_count, 1);
    }

    #[test]
    fn source_metadata_rejects_bad_identity_order_minima_and_counters() {
        let request = fixture_request(2, true);
        let output = fixture_quote(&request);
        for mutation in 0..14 {
            let mut bad = output.clone();
            match mutation {
                0 => bad.repository = RepositoryId::from_bytes([9; 16]),
                1 => bad.generation = GenerationId::from_bytes([9; 20]),
                2 => bad.quote_policy_version += 1,
                3 => bad.entries.swap(0, 1),
                4 => {
                    bad.entries.pop();
                }
                5 => bad.entries.push(bad.entries[0].clone()),
                6 => bad.entries[0].prefix_fixed_upper.json_bytes = 0,
                7 => bad.entries[0].prefix_fixed_upper.memory_bytes = 0,
                8 => bad.entries[1].prefix_fixed_upper.rows = 1,
                9 => bad.entries[1].prefix_fixed_upper.paths = 1,
                10 => bad.entries[1].prefix_fixed_upper.traversal_facts = 0,
                11 => bad.usage.source_bytes = 1,
                12 => bad.usage.results -= 1,
                13 => bad.usage.results += 1,
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    SourceQuoteBounds::new(&request, &bad),
                    Err(ContextEvidenceCollectionError::InvalidProviderResponse)
                ),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn source_metadata_overflow_omits_instead_of_wrapping_reservation() {
        let request = fixture_request(1, true);
        let mut output = fixture_quote(&request);
        output.entries[0].prefix_fixed_upper.json_bytes = u64::MAX;
        output.entries[0].prefix_fixed_upper.memory_bytes = u64::MAX;
        let result = SourceQuoteBounds::new(&request, &output);
        if usize::BITS == 64 {
            let bounds = result.unwrap();
            assert!(bounds.provider(1, 64).is_none());
            assert_eq!(
                bounds
                    .affordable(BudgetLedger::new(None).remaining(), 8192, true)
                    .target_count,
                0
            );
        } else {
            assert!(result.is_err());
        }
    }

    #[test]
    fn source_metadata_shaping_counts_full_ref_without_owned_json_buffer() {
        let request = fixture_request(1, true);
        for snippets in [false, true] {
            let reference = &request.targets[0].source_ref;
            let expected = serde_json::to_vec(&ShapingMetadata {
                source_ref: reference,
                signature: (!snippets).then_some(""),
                snippet: snippets.then(|| rootlight_mcp_contract::context::RepositorySnippet {
                    source_ref: reference.clone(),
                    content: String::new(),
                    language: String::new(),
                    provenance: rootlight_mcp_contract::context::SnippetProvenance::SourceRead,
                    truncated: false,
                    trust: rootlight_mcp_contract::TrustClassification::UntrustedRepositoryData,
                }),
            })
            .unwrap();
            let counted = shaping_fixed(reference, snippets).unwrap();
            assert_eq!(counted.json_bytes, u64::try_from(expected.len()).unwrap());
            assert_eq!(counted.tokens, counted.json_bytes);
            assert_eq!(counted.memory_bytes, counted.json_bytes);
        }
    }

    #[derive(Debug, Clone, Default)]
    struct QuoteCancellation(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl CancellationSignal for QuoteCancellation {
        fn is_cancelled(&self) -> bool {
            self.0.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[derive(Debug)]
    struct QuotePort {
        response: Result<
            ContextSourceMetadataQuoteOutput,
            crate::context_evidence::ContextEvidencePortError,
        >,
        interrupt: bool,
        observed: std::sync::Arc<std::sync::Mutex<Vec<BudgetCharge>>>,
    }

    impl ContextEvidencePort<QuoteCancellation> for QuotePort {
        fn retrieve(
            &self,
            _invocation: crate::context_evidence::EvidenceProviderInvocation,
            _context: ContextEvidenceCallContext<QuoteCancellation>,
        ) -> crate::port::AgentPortFuture<
            Result<
                crate::context_evidence::EvidenceProviderOutput,
                crate::context_evidence::ContextEvidencePortError,
            >,
        > {
            Box::pin(async {
                Err(crate::context_evidence::ContextEvidencePortError {
                    kind: ContextEvidencePortErrorKind::Unsupported,
                    usage: BudgetCharge::default(),
                })
            })
        }

        fn quote_source_metadata(
            &self,
            _request: ContextSourceMetadataQuoteRequest,
            context: ContextEvidenceCallContext<QuoteCancellation>,
        ) -> crate::port::AgentPortFuture<
            Result<
                ContextSourceMetadataQuoteOutput,
                crate::context_evidence::ContextEvidencePortError,
            >,
        > {
            self.observed.lock().unwrap().push(context.reservation());
            let response = self.response.clone();
            let signal = context.cancellation().clone();
            let interrupt = self.interrupt;
            Box::pin(async move {
                if interrupt {
                    signal.0.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                response
            })
        }
    }

    fn quote_work() -> BudgetCharge {
        BudgetCharge {
            rows: 17,
            results: 1,
            tokens: 19,
            actual_tokens: 3,
            source_bytes: 0,
            traversal_facts: 11,
            depth: 2,
            paths: 4,
            json_bytes: 211,
            memory_bytes: 345,
            time_ms: 7,
        }
    }

    fn quote_port(
        response: Result<
            ContextSourceMetadataQuoteOutput,
            crate::context_evidence::ContextEvidencePortError,
        >,
        interrupt: bool,
    ) -> QuotePort {
        QuotePort {
            response,
            interrupt,
            observed: Default::default(),
        }
    }

    #[tokio::test]
    async fn source_metadata_received_work_commits_once_before_stop_or_validation() {
        for invalid in [false, true] {
            for interrupted in [false, true] {
                let request = fixture_request(1, true);
                let mut output = fixture_quote(&request);
                output.usage = quote_work();
                if invalid {
                    output.quote_policy_version += 1;
                }
                let port = quote_port(Ok(output), interrupted);
                let mut ledger = BudgetLedger::with_token_limit(20_000);
                let before = ledger.remaining();
                let result = quote_bounds(
                    &port,
                    request,
                    &mut ledger,
                    QuoteCancellation::default(),
                    Instant::now() + std::time::Duration::from_secs(1),
                )
                .await;
                assert_eq!(ledger.consumed(), quote_work());
                assert_eq!(ledger.snapshot().reserved(), BudgetCharge::default());
                assert_eq!(*port.observed.lock().unwrap(), vec![before]);
                if interrupted {
                    assert!(matches!(
                        result,
                        Err(ContextEvidencePlanningError::Collection(
                            ContextEvidenceCollectionError::Cancelled
                        ))
                    ));
                } else if invalid {
                    assert!(matches!(
                        result,
                        Err(ContextEvidencePlanningError::Collection(
                            ContextEvidenceCollectionError::InvalidProviderResponse
                        ))
                    ));
                } else {
                    assert!(matches!(result, Ok(QuoteBoundsOutcome::Complete(_))));
                    assert_eq!(ledger.remaining().source_bytes, before.source_bytes);
                }
            }
        }
    }

    #[tokio::test]
    async fn source_metadata_malformed_completeness_rejects_after_received_work() {
        let request = fixture_request(1, true);
        let mut output = fixture_quote(&request);
        output.usage = quote_work();
        output.completeness.continuation =
            rootlight_mcp_contract::completeness::ContinuationAvailability::Unavailable;
        assert!(SourceQuoteBounds::new(&request, &output).is_err());
        let port = quote_port(Ok(output), false);
        let mut ledger = BudgetLedger::with_token_limit(20_000);
        assert!(matches!(
            quote_bounds(
                &port,
                request,
                &mut ledger,
                QuoteCancellation::default(),
                Instant::now() + std::time::Duration::from_secs(1)
            )
            .await,
            Err(ContextEvidencePlanningError::Collection(
                ContextEvidenceCollectionError::InvalidProviderResponse
            ))
        ));
        assert_eq!(ledger.consumed(), quote_work());
        assert_eq!(ledger.snapshot().reserved(), BudgetCharge::default());
    }

    #[tokio::test]
    async fn source_metadata_partial_completeness_forwards_checked_canonical_value() {
        use rootlight_mcp_contract::completeness::{
            CompletenessState, ContinuationAvailability, ContinuationGuidance, LimitingResource,
            LimitingResourceKind,
        };
        let request = fixture_request(1, true);
        let mut output = fixture_quote(&request);
        output.usage = quote_work();
        output.completeness = ResultCompleteness {
            state: CompletenessState::Truncated,
            limiting_resources: vec![
                LimitingResource::kind(LimitingResourceKind::Paths),
                LimitingResource::kind(LimitingResourceKind::Rows),
            ],
            continuation: ContinuationAvailability::Unavailable,
            guidance: vec![
                ContinuationGuidance::SplitRequest,
                ContinuationGuidance::NarrowScope,
                ContinuationGuidance::SplitRequest,
            ],
        };
        let expected = ResultCompleteness::new(
            CompletenessState::Truncated,
            vec![
                LimitingResource::kind(LimitingResourceKind::Rows),
                LimitingResource::kind(LimitingResourceKind::Paths),
            ],
            ContinuationAvailability::Unavailable,
            vec![
                ContinuationGuidance::NarrowScope,
                ContinuationGuidance::SplitRequest,
            ],
        )
        .unwrap();
        assert_ne!(output.completeness, expected);
        let port = quote_port(Ok(output), false);
        let mut ledger = BudgetLedger::with_token_limit(20_000);
        let result = quote_bounds(
            &port,
            request,
            &mut ledger,
            QuoteCancellation::default(),
            Instant::now() + std::time::Duration::from_secs(1),
        )
        .await
        .unwrap();
        let QuoteBoundsOutcome::Omitted(label, completeness) = result else {
            panic!("partial metadata must not authorize preview bounds");
        };
        assert_eq!(label, "source_metadata_incomplete");
        assert_eq!(completeness, expected);
        assert_eq!(ledger.consumed(), quote_work());
        assert_eq!(ledger.snapshot().reserved(), BudgetCharge::default());
    }

    #[tokio::test]
    async fn source_metadata_unavailable_incomplete_and_interrupted_errors_keep_receipts() {
        for kind in [
            ContextEvidencePortErrorKind::Unsupported,
            ContextEvidencePortErrorKind::Unavailable,
        ] {
            for interrupted in [false, true] {
                let port = quote_port(
                    Err(crate::context_evidence::ContextEvidencePortError {
                        kind,
                        usage: quote_work(),
                    }),
                    interrupted,
                );
                let mut ledger = BudgetLedger::with_token_limit(20_000);
                let result = quote_bounds(
                    &port,
                    fixture_request(1, true),
                    &mut ledger,
                    QuoteCancellation::default(),
                    Instant::now() + std::time::Duration::from_secs(1),
                )
                .await;
                assert_eq!(ledger.consumed(), quote_work());
                assert_eq!(ledger.snapshot().reserved(), BudgetCharge::default());
                if interrupted {
                    assert!(matches!(
                        result,
                        Err(ContextEvidencePlanningError::Collection(
                            ContextEvidenceCollectionError::Cancelled
                        ))
                    ));
                } else {
                    assert!(matches!(result, Ok(QuoteBoundsOutcome::Omitted(_, _))));
                }
            }
        }
        let request = fixture_request(1, true);
        let mut output = fixture_quote(&request);
        output.completeness =
            source_completeness(ContextEvidencePortErrorKind::Unavailable, false).unwrap();
        output.entries.clear();
        output.usage = quote_work();
        let port = quote_port(Ok(output), false);
        let mut ledger = BudgetLedger::with_token_limit(20_000);
        assert!(matches!(
            quote_bounds(
                &port,
                request,
                &mut ledger,
                QuoteCancellation::default(),
                Instant::now() + std::time::Duration::from_secs(1)
            )
            .await,
            Ok(QuoteBoundsOutcome::Omitted("source_metadata_incomplete", _))
        ));
        assert_eq!(ledger.consumed(), quote_work());
    }

    #[tokio::test]
    async fn source_metadata_received_overage_wins_over_same_poll_cancellation() {
        for failed in [false, true] {
            let request = fixture_request(1, true);
            let mut output = fixture_quote(&request);
            let mut ledger = BudgetLedger::with_token_limit(20_000);
            output.usage = BudgetCharge {
                rows: ledger.remaining().rows + 1,
                ..quote_work()
            };
            let response = if failed {
                Err(crate::context_evidence::ContextEvidencePortError {
                    kind: ContextEvidencePortErrorKind::Unavailable,
                    usage: output.usage,
                })
            } else {
                Ok(output)
            };
            let port = quote_port(response, true);
            let result = quote_bounds(
                &port,
                request,
                &mut ledger,
                QuoteCancellation::default(),
                Instant::now() + std::time::Duration::from_secs(1),
            )
            .await;
            assert!(matches!(
                result,
                Err(ContextEvidencePlanningError::Planning(
                    ContextPackPlanningError::Policy(
                        crate::policy::ExecutionPolicyError::BudgetExceeded {
                            resource: crate::policy::BudgetResource::Rows,
                        }
                    )
                ))
            ));
            assert_eq!(ledger.snapshot().reserved(), BudgetCharge::default());
        }
    }

    #[tokio::test]
    async fn source_metadata_exhausted_legal_capacity_omits_without_io_but_checks_stop() {
        let request = fixture_request(1, true);
        let port = quote_port(Ok(fixture_quote(&request)), false);
        let mut ledger = BudgetLedger::with_token_limit(20_000);
        ledger
            .charge(BudgetCharge {
                source_bytes: ledger.remaining().source_bytes,
                ..BudgetCharge::default()
            })
            .unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        assert!(matches!(
            quote_bounds(
                &port,
                request.clone(),
                &mut ledger,
                QuoteCancellation::default(),
                deadline
            )
            .await,
            Ok(QuoteBoundsOutcome::Omitted("source_budget", _))
        ));
        assert!(port.observed.lock().unwrap().is_empty());
        let cancellation = QuoteCancellation::default();
        cancellation
            .0
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(matches!(
            quote_bounds(&port, request.clone(), &mut ledger, cancellation, deadline).await,
            Err(ContextEvidencePlanningError::Collection(
                ContextEvidenceCollectionError::Cancelled
            ))
        ));
        assert!(matches!(
            quote_bounds(
                &port,
                request,
                &mut ledger,
                QuoteCancellation::default(),
                Instant::now()
            )
            .await,
            Err(ContextEvidencePlanningError::Collection(
                ContextEvidenceCollectionError::DeadlineExceeded
            ))
        ));
        assert!(port.observed.lock().unwrap().is_empty());
    }
}
