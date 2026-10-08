//! Resident metadata quotes for a later lossless prefix read.
//! Bounds do not authenticate source text or imply snapshot freshness.

use super::*;

/// Cumulative fixed costs for one ordered prefix, excluding returned text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct SourceTextPrefixFixedUpper {
    /// Escaped JSON metadata upper bound.
    pub json_bytes: u64,
    /// Owned metadata memory upper bound.
    pub memory_bytes: u64,
    /// Logical source and sidecar row upper bound.
    pub rows: u64,
    /// Conditional-activity fact traversal upper bound.
    pub traversal_facts: u64,
    /// Source path count upper bound.
    pub paths: u64,
}

/// One unchanged editing authority and the bound through this ordered entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceTextPrefixMetadataQuoteEntry {
    /// Full approved original-byte selection, including its line hints.
    pub source: SourceReference,
    /// Cumulative fixed costs, not a per-entry average.
    pub prefix_fixed_upper: SourceTextPrefixFixedUpper,
}

/// Checked metadata-only quote; mutable DTO fields are not persistent proofs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceTextPrefixMetadataQuoteResult {
    /// Exact resident generation and measured quote work.
    pub context: QueryContext,
    /// Supported bound derivation policy.
    pub quote_policy_version: u32,
    /// Ordered cumulative bounds retaining the complete requested references.
    pub entries: Vec<SourceTextPrefixMetadataQuoteEntry>,
    /// Execution completeness, independent of later source-read completeness.
    pub execution_completeness: ResultCompleteness,
}

impl Client {
    /// Quotes resident fixed metadata without reading or validating source bodies.
    ///
    /// Only no context or one line on both sides is supported. Merging and
    /// returned line numbers must be disabled; references remain unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid controls, unavailable protocol 1.26,
    /// transport failure, or uncorrelated metadata bounds.
    pub fn source_text_prefix_metadata_quote_with_options(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
        references: &[SourceReference],
        projection: SourceTextReadOptions,
        options: RequestOptions,
    ) -> Result<SourceTextPrefixMetadataQuoteResult, ClientError> {
        let (response, minor) = self.request_with_protocol_and_options(
            build_metadata_quote_request(repository, generation, references, projection)?,
            options,
        )?;
        match response {
            daemon::response_envelope::Response::SourceTextPrefixMetadataQuote(response) => {
                parse_metadata_quote(response, repository, generation, references, options, minor)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Asynchronously quotes fixed costs for an ordered immutable-source prefix.
    ///
    /// Dropping the future closes its one-request stream and permits cancellation.
    ///
    /// # Panics
    ///
    /// Panics if polled without Tokio's time or I/O drivers enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid controls, unavailable protocol support,
    /// transport failure, timeout, or inconsistent metadata bounds.
    pub async fn source_text_prefix_metadata_quote_async_with_options(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
        references: &[SourceReference],
        projection: SourceTextReadOptions,
        options: RequestOptions,
    ) -> Result<SourceTextPrefixMetadataQuoteResult, ClientError> {
        let (response, minor) = self
            .request_async_with_options_and_protocol(
                build_metadata_quote_request(repository, generation, references, projection)?,
                options,
            )
            .await?;
        match response {
            daemon::response_envelope::Response::SourceTextPrefixMetadataQuote(response) => {
                parse_metadata_quote(response, repository, generation, references, options, minor)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }
}

fn build_metadata_quote_request(
    repository: RepositoryId,
    generation: GenerationId,
    references: &[SourceReference],
    projection: SourceTextReadOptions,
) -> Result<daemon::request_envelope::Request, ClientError> {
    if projection.merge_overlaps
        || projection.include_line_numbers
        || !matches!(
            (
                projection.context_lines_before,
                projection.context_lines_after
            ),
            (0, 0) | (1, 1)
        )
        || references.is_empty()
        || references.len() > 32
        || references.iter().enumerate().any(|(index, source)| {
            source.repository() != repository
                || source.generation() != generation
                || references[..index].contains(source)
        })
    {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    Ok(
        daemon::request_envelope::Request::SourceTextPrefixMetadataQuote(
            daemon::SourceTextPrefixMetadataQuoteRequest {
                schema_version: Some(first_slice_schema()),
                repository: Some(repository_to_wire(repository)),
                generation: Some(generation_to_wire(generation)),
                references: references.iter().map(source_reference_to_wire).collect(),
                context_lines_before: u32::from(projection.context_lines_before),
                context_lines_after: u32::from(projection.context_lines_after),
            },
        ),
    )
}

fn parse_metadata_quote(
    response: daemon::SourceTextPrefixMetadataQuoteResponse,
    repository: RepositoryId,
    generation: GenerationId,
    requested: &[SourceReference],
    options: RequestOptions,
    minor: u32,
) -> Result<SourceTextPrefixMetadataQuoteResult, ClientError> {
    if minor < rootlight_protocol::SOURCE_TEXT_PREFIX_METADATA_QUOTE_PROTOCOL_MINOR {
        return Err(ClientError::ProtocolFeatureUnavailable);
    }
    require_first_slice_response_schema(response.schema_version)?;
    if response.quote_policy_version
        != rootlight_protocol::SOURCE_TEXT_PREFIX_METADATA_QUOTE_POLICY_VERSION
        || response.completeness.is_none()
        || requested.is_empty()
        || requested.len() > 32
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let execution_completeness = parse_result_completeness(response.completeness, None, false)?;
    if !matches!(
        execution_completeness.state,
        ResultCompletenessState::Complete | ResultCompletenessState::Truncated
    ) {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let complete = execution_completeness.state == ResultCompletenessState::Complete;
    if response.entries.len() > requested.len()
        || (complete && response.entries.len() != requested.len())
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let context = parse_query_context(
        response.context,
        repository,
        GenerationSelector::Generation(generation),
    )?;
    let usage = context.usage;
    if usage.source_bytes != 0
        || usage.results
            != u64::try_from(response.entries.len())
                .map_err(|_| ClientError::InvalidResponseCorrelation)?
        || usage.memory_bytes.is_none()
        || usage.rows > MAX_FIRST_SLICE_BUDGET_ROWS
        || usage.edges > MAX_FIRST_SLICE_BUDGET_EDGES
        || usage.results > MAX_FIRST_SLICE_BUDGET_RESULTS
        || usage.json_bytes > MAX_FIRST_SLICE_BUDGET_JSON_BYTES
        || usage.estimated_tokens > MAX_FIRST_SLICE_BUDGET_ESTIMATED_TOKENS
        || usage
            .memory_bytes
            .is_some_and(|bytes| bytes > MAX_FIRST_SLICE_BUDGET_MEMORY_BYTES)
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    if let Some(budget) = options.effective_budget() {
        let limits = budget.limits();
        if usage.rows > limits.rows
            || usage.edges > limits.edges
            || usage.results > limits.results
            || usage.json_bytes > limits.json_bytes
            || usage.estimated_tokens > limits.estimated_tokens
            || usage
                .memory_bytes
                .is_some_and(|bytes| bytes > limits.memory_bytes)
            || u128::from(usage.elapsed_micros) > limits.duration.as_micros()
        {
            return Err(ClientError::InvalidResponseCorrelation);
        }
    }
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(response.entries.len())
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    let mut previous = [0_u64; 5];
    for (index, (entry, expected)) in response.entries.into_iter().zip(requested).enumerate() {
        let source = parse_source_reference(
            entry
                .source
                .ok_or(ClientError::InvalidResponseCorrelation)?,
            &context,
        )?;
        let upper = entry
            .prefix_fixed_upper
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        let values = [
            upper.json_bytes,
            upper.memory_bytes,
            upper.rows,
            upper.traversal_facts,
            upper.paths,
        ];
        // Future demand may exceed this call's budget; only quote execution is
        // bounded by it. Never clamp demand into an affordable-looking quote.
        let count =
            u64::try_from(index + 1).map_err(|_| ClientError::InvalidResponseCorrelation)?;
        if source != *expected
            || upper.json_bytes == 0
            || upper.memory_bytes == 0
            || upper.rows < count
            || upper.paths < count
            || values
                .iter()
                .zip(previous)
                .any(|(value, old)| value.checked_sub(old).is_none())
            || usize::try_from(upper.json_bytes).is_err()
            || usize::try_from(upper.memory_bytes).is_err()
        {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        previous = values;
        entries.push(SourceTextPrefixMetadataQuoteEntry {
            source,
            prefix_fixed_upper: SourceTextPrefixFixedUpper {
                json_bytes: upper.json_bytes,
                memory_bytes: upper.memory_bytes,
                rows: upper.rows,
                traversal_facts: upper.traversal_facts,
                paths: upper.paths,
            },
        });
    }
    Ok(SourceTextPrefixMetadataQuoteResult {
        context,
        quote_policy_version: response.quote_policy_version,
        entries,
        execution_completeness,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{test_generation, test_repository, test_source, wire_query_context};

    fn projection() -> SourceTextReadOptions {
        SourceTextReadOptions {
            include_line_numbers: false,
            ..SourceTextReadOptions::new()
        }
    }

    fn fixture(sources: &[SourceReference]) -> daemon::SourceTextPrefixMetadataQuoteResponse {
        let mut context = wire_query_context(u64::try_from(sources.len()).unwrap(), 0);
        context.usage.as_mut().unwrap().memory_bytes = Some(128);
        daemon::SourceTextPrefixMetadataQuoteResponse {
            schema_version: Some(first_slice_schema()),
            context: Some(context),
            quote_policy_version: 1,
            entries: sources
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    let count = u64::try_from(index + 1).unwrap();
                    daemon::SourceTextPrefixMetadataQuoteEntry {
                        source: Some(source_reference_to_wire(source)),
                        prefix_fixed_upper: Some(daemon::SourceTextPrefixFixedUpper {
                            json_bytes: count * 1024,
                            memory_bytes: count * 512,
                            rows: count,
                            traversal_facts: count * 2,
                            paths: count,
                        }),
                    }
                })
                .collect(),
            completeness: Some(daemon::FirstSliceCompleteness {
                state: daemon::FirstSliceCompletenessState::FirstSliceCompletenessComplete as i32,
                continuation:
                    daemon::FirstSliceContinuationAvailability::FirstSliceContinuationNotApplicable
                        as i32,
                limiting_resources: Vec::new(),
                guidance: Vec::new(),
            }),
        }
    }

    fn parse(
        response: daemon::SourceTextPrefixMetadataQuoteResponse,
        sources: &[SourceReference],
    ) -> Result<SourceTextPrefixMetadataQuoteResult, ClientError> {
        parse_metadata_quote(
            response,
            test_repository(),
            test_generation(),
            sources,
            RequestOptions::new(),
            26,
        )
    }

    #[test]
    fn metadata_quote_admission_requires_minor_and_exact_supported_controls() {
        let sources = [test_source(4, 0, 3), test_source(5, 3, 9)];
        let request = build_metadata_quote_request(
            test_repository(),
            test_generation(),
            &sources,
            projection(),
        )
        .unwrap();
        assert!(matches!(
            ensure_request_supported(&request, 25),
            Err(ClientError::ProtocolFeatureUnavailable)
        ));
        assert!(ensure_request_supported(&request, 26).is_ok());
        for (before, after) in [(0, 1), (1, 0), (2, 2)] {
            let controls = SourceTextReadOptions {
                context_lines_before: before,
                context_lines_after: after,
                ..projection()
            };
            assert!(
                build_metadata_quote_request(
                    test_repository(),
                    test_generation(),
                    &sources,
                    controls
                )
                .is_err()
            );
        }
        assert!(
            build_metadata_quote_request(
                test_repository(),
                test_generation(),
                &[sources[0].clone(), sources[0].clone()],
                projection()
            )
            .is_err()
        );
        assert!(
            build_metadata_quote_request(test_repository(), test_generation(), &[], projection())
                .is_err()
        );
    }

    #[test]
    fn metadata_quote_retains_order_full_authority_and_cumulative_bounds() {
        let sources = [test_source(4, 0, 3), test_source(5, 3, 9)];
        let quote = parse(fixture(&sources), &sources).unwrap();
        assert_eq!(quote.context.usage.source_bytes, 0);
        assert_eq!(quote.entries[1].source, sources[1]);
        assert_eq!(quote.entries[1].prefix_fixed_upper.json_bytes, 2048);
        assert_eq!(quote.entries[1].prefix_fixed_upper.rows, 2);
    }

    #[test]
    fn metadata_quote_rejects_policy_order_cost_and_usage_tampering() {
        let sources = [test_source(4, 0, 3), test_source(5, 3, 9)];
        let check = |wire| {
            assert!(matches!(
                parse(wire, &sources),
                Err(ClientError::InvalidResponseCorrelation)
            ))
        };
        let mut wire = fixture(&sources);
        wire.quote_policy_version = 2;
        check(wire);
        let mut wire = fixture(&sources);
        wire.entries.swap(0, 1);
        check(wire);
        let mut wire = fixture(&sources);
        wire.entries.pop();
        check(wire);
        let mut wire = fixture(&sources);
        wire.entries[1]
            .prefix_fixed_upper
            .as_mut()
            .unwrap()
            .traversal_facts = 1;
        check(wire);
        let mut wire = fixture(&sources);
        wire.entries[0].source.as_mut().unwrap().start_line = Some(2);
        check(wire);
        let mut wire = fixture(&sources);
        wire.context
            .as_mut()
            .unwrap()
            .usage
            .as_mut()
            .unwrap()
            .source_bytes = 1;
        check(wire);
        let mut wire = fixture(&sources);
        wire.completeness = None;
        check(wire);
        let mut wire = fixture(&sources);
        wire.entries[0]
            .prefix_fixed_upper
            .as_mut()
            .unwrap()
            .json_bytes = 0;
        check(wire);
        let mut wire = fixture(&sources);
        wire.entries[1].prefix_fixed_upper.as_mut().unwrap().paths = 1;
        check(wire);
        let mut wire = fixture(&sources);
        wire.context
            .as_mut()
            .unwrap()
            .usage
            .as_mut()
            .unwrap()
            .results = 1;
        check(wire);
    }

    #[test]
    fn metadata_quote_demand_is_not_clamped_to_execution_budget() {
        let sources = [test_source(4, 0, 3)];
        let budget = EffectiveBudget::new(EffectiveBudgetLimits {
            rows: 1,
            edges: 1,
            results: 1,
            source_bytes: 1,
            json_bytes: 128,
            estimated_tokens: 128,
            memory_bytes: 128,
            duration: Duration::from_secs(1),
            depth: None,
            paths: None,
        })
        .unwrap();
        let options = RequestOptions::new().with_effective_budget(budget);
        let wire = fixture(&sources);
        let parsed = parse_metadata_quote(
            wire.clone(),
            test_repository(),
            test_generation(),
            &sources,
            options,
            26,
        )
        .unwrap();
        assert_eq!(parsed.entries[0].prefix_fixed_upper.json_bytes, 1024);
        let mut oversized_usage = wire;
        oversized_usage
            .context
            .as_mut()
            .unwrap()
            .usage
            .as_mut()
            .unwrap()
            .memory_bytes = Some(129);
        assert!(matches!(
            parse_metadata_quote(
                oversized_usage,
                test_repository(),
                test_generation(),
                &sources,
                options,
                26
            ),
            Err(ClientError::InvalidResponseCorrelation)
        ));
    }

    #[test]
    fn metadata_quote_partial_prefix_keeps_typed_execution_truncation() {
        let sources = [test_source(4, 0, 3), test_source(5, 3, 9)];
        let mut wire = fixture(&sources);
        wire.entries.truncate(1);
        wire.context
            .as_mut()
            .unwrap()
            .usage
            .as_mut()
            .unwrap()
            .results = 1;
        wire.completeness = Some(daemon::FirstSliceCompleteness {
            state: daemon::FirstSliceCompletenessState::FirstSliceCompletenessTruncated as i32,
            continuation:
                daemon::FirstSliceContinuationAvailability::FirstSliceContinuationUnavailable as i32,
            limiting_resources: vec![daemon::FirstSliceLimitingResource {
                kind: daemon::FirstSliceLimitingResourceKind::FirstSliceLimitRows as i32,
                limit: Some(1),
                observed: Some(1),
            }],
            guidance: vec![
                daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceSplitRequest as i32,
            ],
        });
        let result = parse(wire, &sources).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].source, sources[0]);
        assert_eq!(
            result.execution_completeness.state,
            ResultCompletenessState::Truncated
        );
        assert_eq!(result.context.usage.rows, 2);
        assert_eq!(result.context.usage.source_bytes, 0);
    }
}
