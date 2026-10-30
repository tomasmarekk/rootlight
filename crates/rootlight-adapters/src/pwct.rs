//! Emit the independently reproducible native PWCT visual-source census.
//! The shared SDK model authenticates written IDs and physical row/token spans;
//! visual ownership and embedded Ring semantics remain consumer responsibilities.

use rootlight_adapter_sdk::{
    AdapterError, CoverageReport, DescriptorError, DiagnosticCode, DomainCoverage, EncodingId,
    LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider, ParseReport, ParseRequest,
    RequestError, ResourceUsage, SinkError, StreamEnd, SyntaxFact, SyntaxFactBatch, SyntaxFactSink,
    SyntaxKindLabel, pwct,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;
use rootlight_ir::{AnalysisTier, CoverageStatus, FactDomain, SourceSpan};

/// Native inert provider for complete PWCT 1.0.0 RingPWCT visual-source files.
pub struct PwctParseProvider {
    capabilities: ParseCapabilities,
}

impl PwctParseProvider {
    /// Creates a UTF-8 full-source provider under caller-owned byte and raw-node limits.
    ///
    /// # Errors
    /// Returns [`DescriptorError`] for a zero source or syntax-node ceiling.
    ///
    /// # Panics
    /// Panics only if fixed SDK language/encoding labels violate their contracts.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        Ok(Self {
            capabilities: ParseCapabilities::new(
                vec![LanguageId::new("pwct").expect("fixed PWCT language")],
                vec![EncodingId::utf8()],
                max_source_bytes,
                max_syntax_nodes,
                64,
                0,
                false,
                false,
                true,
                1,
                MemoryEnforcement::Unavailable,
            )?,
        })
    }

    /// Binds the native wrapper and complete shared model source to the frontend identity.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        let mut hash = blake3::Hasher::new();
        hash.update(b"rootlight-pwct-visual-source-plan/1\0");
        hash.update(include_bytes!("pwct.rs"));
        hash.update(pwct::source_digest().as_bytes());
        ContentHash::from_bytes(*hash.finalize().as_bytes())
    }

    fn plan(
        &self,
        request: &ParseRequest<'_>,
        cancel: &Cancellation,
    ) -> Result<pwct::Plan, AdapterError> {
        cancel.check()?;
        if request.language().as_str() != "pwct" {
            return Err(RequestError::UnsupportedLanguage.into());
        }
        if request.encoding().as_str() != "utf-8" {
            return Err(RequestError::UnsupportedEncoding.into());
        }
        if !request.included_ranges().is_empty() {
            return Err(RequestError::EmbeddedRangesUnsupported.into());
        }
        let source = request.source();
        if source.source_ref().span().start_byte() != 0
            || usize::try_from(source.source_ref().span().end_byte()).ok()
                != Some(source.bytes().len())
        {
            return Err(failure("pwct-complete-source-required"));
        }
        let source_bytes = request
            .limits()
            .max_source_bytes()
            .min(self.capabilities.max_source_bytes());
        let maximum_records = request
            .limits()
            .max_syntax_nodes()
            .min(self.capabilities.max_syntax_nodes());
        let limits = pwct::Limits::for_parse(
            source_bytes,
            maximum_records,
            request
                .limits()
                .max_syntax_depth()
                .min(self.capabilities.max_syntax_depth()),
        )
        .map_err(model_error)?;
        pwct::plan(source.bytes(), limits, maximum_records, cancel).map_err(model_error)
    }

    /// Counts the same complete required census used by parsing, without emitting facts.
    ///
    /// # Errors
    /// Returns context, decode, allocation, required-record ceiling, or cancellation refusal.
    pub fn required_syntax_fact_count(
        &self,
        request: &ParseRequest<'_>,
        cancel: &Cancellation,
    ) -> Result<usize, AdapterError> {
        Ok(self.plan(request, cancel)?.records.len())
    }
}

impl ParseProvider for PwctParseProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancel: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let plan = self.plan(request, cancel)?;
        let file = request.source().source_ref().span().file();
        let batch_records = request
            .limits()
            .syntax_stream()
            .batch()
            .max_records()
            .min(32);
        for records in plan.records.chunks(batch_records) {
            cancel.check()?;
            let mut facts = Vec::new();
            facts
                .try_reserve_exact(records.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            for record in records {
                facts.push(SyntaxFact::new(
                    record.local_id,
                    record.parent,
                    record.role.kind(),
                    SourceSpan::new(
                        file,
                        u64::try_from(record.span.start)
                            .map_err(|_| failure("pwct-source-span"))?,
                        u64::try_from(record.span.end).map_err(|_| failure("pwct-source-span"))?,
                    )
                    .map_err(|_| failure("pwct-source-span"))?,
                    record.role.depth(),
                    SyntaxKindLabel::new(record.role.syntax())
                        .map_err(|_| failure("pwct-syntax-label"))?,
                ));
            }
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), facts, Vec::new()),
                cancel,
            )?;
        }
        let usage = sink.staged_usage();
        let source_bytes = request.source().bytes().len();
        let steps = plan.model.steps.len();
        Ok(ParseReport::new(
            CoverageReport::new(
                AnalysisTier::TierD,
                CoverageStatus::Bounded,
                source_bytes,
                source_bytes,
                0,
                vec![
                    DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                    DomainCoverage::new(
                        FactDomain::Entities,
                        CoverageStatus::Bounded,
                        steps,
                        steps,
                        0,
                    )?,
                    DomainCoverage::new(
                        FactDomain::Occurrences,
                        CoverageStatus::Bounded,
                        steps,
                        steps,
                        0,
                    )?,
                    DomainCoverage::new(FactDomain::Relations, CoverageStatus::Unknown, 0, 0, 0)?,
                ],
            )?,
            ResourceUsage::new(
                source_bytes,
                plan.records.len(),
                plan.model.literal_values,
                plan.model.literal_depth,
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

fn failure(code: &'static str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("fixed PWCT refusal"),
    }
}

fn model_error(error: pwct::Error) -> AdapterError {
    match error.kind {
        pwct::ErrorKind::Cancelled(error) => error.into(),
        pwct::ErrorKind::Allocation => SinkError::AllocationFailed.into(),
        pwct::ErrorKind::Limit(_) => failure("pwct-source-model-limit"),
        pwct::ErrorKind::Malformed(_) => failure("pwct-source-malformed"),
        pwct::ErrorKind::Unsupported(_) => failure("pwct-source-unsupported"),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use rootlight_adapter_sdk::{
        AnalysisLimits, BatchThresholds, GenerationBoundSnapshot, IncludedRange,
        MemoryAdmissionPolicy, RemainingBudget, StreamLimits, StreamUsage, execute_parse,
        execute_parse_transaction,
    };
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{GenerationId, derive_repository};
    use rootlight_ir::{IrLimits, SourceRef};
    use rootlight_vfs::{RelativePath, RepositoryRoot};

    use super::*;

    const SOURCE_LIMIT: usize = 4 * 1024 * 1024;

    fn document(code: &str) -> String {
        format!(
            r#"cVisualSourceFileVersion='1.0.0'
cVisualLanguageName='RingPWCT'
cNaturalLanguageName='english'
nStepsID=1
nInteractionsID=1
aStepsTable=[[1,0,[["name","Functions"],["active",1],["code","{code}"],["interactionid",1],["visible",1],["stepnumber",1],["steptype",2]]]]
aInteractionsTable=[[1,1,"print","01/01/2023","12:00:00",""]]
"#
        )
    }

    fn with_file<R>(
        directory: &Path,
        relative: &str,
        nodes: usize,
        records: usize,
        action: impl FnOnce(&ParseRequest<'_>) -> R,
    ) -> R {
        let repository = derive_repository(b"pwct-native-provider-contract").id();
        let root = RepositoryRoot::open(repository, directory).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new(relative)).unwrap(),
                u64::try_from(SOURCE_LIMIT).unwrap(),
            )
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([71; 20]),
            SourceSpan::new(
                snapshot.file(),
                0,
                u64::try_from(snapshot.content().len()).unwrap(),
            )
            .unwrap(),
            snapshot.content_hash(),
            None,
        );
        let stream = StreamLimits::new(
            4096,
            records,
            16 * 1024 * 1024,
            16,
            65536,
            1024 * 1024,
            BatchThresholds::new(2, 1024 * 1024, 1, 4096).unwrap(),
        )
        .unwrap();
        let limits = AnalysisLimits::new(
            SOURCE_LIMIT,
            nodes,
            64,
            1,
            64 * 1024 * 1024,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .unwrap();
        let request = ParseRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("pwct").unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            &limits,
        )
        .unwrap();
        action(&request)
    }

    fn with_source<R>(
        source: &str,
        nodes: usize,
        records: usize,
        action: impl FnOnce(&ParseRequest<'_>) -> R,
    ) -> R {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("visual.pwct"), source).unwrap();
        with_file(directory.path(), "visual.pwct", nodes, records, action)
    }

    fn provider() -> PwctParseProvider {
        PwctParseProvider::new(SOURCE_LIMIT, 100_000).unwrap()
    }

    #[test]
    fn native_provider_emits_exact_physical_census_and_separate_resources() {
        with_source(&document("see 1"), 4096, 32, |request| {
            let provider = provider();
            let output = execute_parse(
                &provider,
                request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(
                provider
                    .required_syntax_fact_count(request, &Cancellation::new())
                    .unwrap(),
                4
            );
            assert_eq!(output.facts().len(), 4);
            let labels: Vec<_> = output
                .facts()
                .iter()
                .map(|fact| fact.syntax_kind().as_str())
                .collect();
            assert_eq!(
                labels,
                [
                    "pwct.source_file",
                    "pwct.step.declaration",
                    "pwct.step.definition",
                    "pwct.step.parent"
                ]
            );
            let facts = output.facts();
            assert_eq!(facts[1].parent(), Some(1));
            assert_eq!((facts[2].parent(), facts[3].parent()), (Some(2), Some(2)));
            assert_eq!(
                (facts[0].depth(), facts[1].depth(), facts[2].depth()),
                (0, 1, 2)
            );
            let bytes = request.source().bytes();
            assert_eq!(
                &bytes[usize::try_from(facts[2].span().start_byte()).unwrap()
                    ..usize::try_from(facts[2].span().end_byte()).unwrap()],
                b"1"
            );
            assert_eq!(
                &bytes[usize::try_from(facts[3].span().start_byte()).unwrap()
                    ..usize::try_from(facts[3].span().end_byte()).unwrap()],
                b"0"
            );
            assert!(output.report().resources().syntax_nodes() > output.facts().len());
            assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
        });
    }

    #[test]
    fn caller_source_ceiling_allows_realistic_files_above_one_mibibyte() {
        let source = document(&"x".repeat(1_048_600));
        with_source(&source, 4096, 32, |request| {
            let output = execute_parse(
                &provider(),
                request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(output.facts().len(), 4);
            assert!(output.report().resources().source_bytes() > 1_048_576);
        });
    }

    #[test]
    fn syntax_and_stream_budgets_refuse_without_a_committed_prefix() {
        with_source(&document("see 1"), 3, 32, |request| {
            assert!(matches!(
                execute_parse(
                    &provider(),
                    request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new()
                ),
                Err(AdapterError::ProviderFailed { .. })
            ));
        });
        with_source(&document("see 1"), 4096, 3, |request| {
            assert_eq!(
                provider()
                    .required_syntax_fact_count(request, &Cancellation::new())
                    .unwrap(),
                4
            );
            assert!(matches!(
                execute_parse(
                    &provider(),
                    request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new()
                ),
                Err(AdapterError::Sink(_))
            ));
        });
    }

    #[test]
    fn wrong_context_and_outer_execution_are_refused_before_publication() {
        with_source(&document("see 1"), 4096, 32, |request| {
            let provider = provider();
            let wrong_language = ParseRequest::new(
                request.source().clone(),
                LanguageId::new("ring").unwrap(),
                EncodingId::utf8(),
                Vec::new(),
                request.limits(),
            )
            .unwrap();
            assert!(matches!(
                provider.required_syntax_fact_count(&wrong_language, &Cancellation::new()),
                Err(AdapterError::RejectedRequest(
                    RequestError::UnsupportedLanguage
                ))
            ));
            let ranged = ParseRequest::new(
                request.source().clone(),
                LanguageId::new("pwct").unwrap(),
                EncodingId::utf8(),
                vec![IncludedRange::new(
                    request.source().source_ref().span(),
                    LanguageId::new("pwct").unwrap(),
                )],
                request.limits(),
            )
            .unwrap();
            assert!(matches!(
                provider.required_syntax_fact_count(&ranged, &Cancellation::new()),
                Err(AdapterError::RejectedRequest(
                    RequestError::EmbeddedRangesUnsupported
                ))
            ));
        });
        let source = format!("{}eval('see 1')\n", document("see 1"));
        with_source(&source, 4096, 32, |request| {
            assert!(matches!(
                execute_parse(
                    &provider(),
                    request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new()
                ),
                Err(AdapterError::ProviderFailed { .. })
            ));
        });
    }

    struct CancelAfterPush<'a> {
        inner: &'a mut dyn SyntaxFactSink,
        cancel: &'a Cancellation,
        pushes: usize,
    }

    impl SyntaxFactSink for CancelAfterPush<'_> {
        fn remaining_budget(&self) -> RemainingBudget {
            self.inner.remaining_budget()
        }
        fn staged_usage(&self) -> StreamUsage {
            self.inner.staged_usage()
        }
        fn next_sequence(&self) -> u64 {
            self.inner.next_sequence()
        }
        fn push(&mut self, batch: SyntaxFactBatch) -> Result<(), SinkError> {
            self.inner.push(batch)?;
            self.pushes += 1;
            self.cancel.cancel(CancellationReason::ClientRequest);
            Ok(())
        }
    }

    #[test]
    fn cancellation_after_the_first_real_batch_discards_the_transaction() {
        with_source(&document("see 1"), 4096, 32, |request| {
            let provider = provider();
            let cancel = Cancellation::new();
            let mut pushes = 0;
            let result = execute_parse_transaction(
                provider.capabilities(),
                request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
                |sink, cancellation| {
                    let mut cancelling = CancelAfterPush {
                        inner: sink,
                        cancel: cancellation,
                        pushes: 0,
                    };
                    let result = provider.parse(request, &mut cancelling, cancellation);
                    pushes = cancelling.pushes;
                    result.map(|report| (report, ()))
                },
            );
            assert_eq!(pushes, 1);
            assert!(matches!(
                result,
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
        });
    }

    #[test]
    #[ignore = "requires the independently pinned original PWCT2 checkout"]
    fn native_provider_originals_match_the_independent_shared_census() {
        let root = std::env::var_os("ROOTLIGHT_PWCT_REPOSITORY")
            .expect("set ROOTLIGHT_PWCT_REPOSITORY to the pinned original checkout");
        for (relative, expected_steps, expected_records) in [
            ("applications/FromPWCTDocs/program9.pwct", 2, 7),
            ("applications/FromPWCTDocs/program3.pwct", 7, 22),
            ("applications/FromPWCTVideos/lesson32_part2.pwct", 3, 10),
            ("applications/Cards/cards.pwct", 452, 1357),
        ] {
            with_file(Path::new(&root), relative, 100_000, 4096, |request| {
                let provider = provider();
                let output = execute_parse(
                    &provider,
                    request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new(),
                )
                .unwrap();
                let limits = pwct::Limits::for_parse(
                    request.limits().max_source_bytes(),
                    request.limits().max_syntax_nodes(),
                    request.limits().max_syntax_depth(),
                )
                .unwrap();
                let census = pwct::plan(
                    request.source().bytes(),
                    limits,
                    request.limits().max_syntax_nodes(),
                    &Cancellation::new(),
                )
                .unwrap();
                assert_eq!(census.model.steps.len(), expected_steps);
                assert_eq!(output.facts().len(), expected_records);
                assert_eq!(
                    provider
                        .required_syntax_fact_count(request, &Cancellation::new())
                        .unwrap(),
                    expected_records
                );
                for record in &census.records {
                    let fact = output
                        .facts()
                        .iter()
                        .find(|fact| fact.local_id() == record.local_id)
                        .unwrap();
                    assert_eq!(
                        (
                            fact.kind(),
                            fact.syntax_kind().as_str(),
                            fact.parent(),
                            fact.depth()
                        ),
                        (
                            record.role.kind(),
                            record.role.syntax(),
                            record.parent,
                            record.role.depth()
                        )
                    );
                    assert_eq!(
                        (fact.span().start_byte(), fact.span().end_byte()),
                        (
                            u64::try_from(record.span.start).unwrap(),
                            u64::try_from(record.span.end).unwrap()
                        )
                    );
                }
                println!(
                    "{relative}: native PWCT facts={expected_records}, visual steps={expected_steps}; Ring semantics remain pending"
                );
            });
        }
    }
}
