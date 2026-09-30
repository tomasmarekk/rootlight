//! Public SDK contracts for the closed first-party procedural VBA route.
//! Native fallback remains whole-module; source identity and resource admission
//! must not change when a file crosses between the two parser routes.

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, IncludedRange, LanguageId, MemoryAdmissionPolicy, NativeVbaModuleKind,
    ParseOutput, ParseRequest, ResourceKind, SinkError, StreamLimits, execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, ReuseInvalidation, ReuseStatus, RuntimeConfig, SourceEdit, TreeSitterProvider,
    VBA_PROCEDURAL_FRONTEND_VERSION,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, IrLimits, SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const SOURCE_LIMIT: usize = 1024 * 1024;
const CLOSED_MODULE: &[u8] = b"Attribute VB_Name = \"Authored\"\r\nOption Explicit\r\nPublic Sub Run()\r\n' only authored trivia\r\nEnd Sub\r\nPrivate Function Count() As Long\r\nEnd Function\r\n";

#[test]
fn accepted_modules_preserve_exact_identity_without_native_pool_or_cache_work() {
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(CLOSED_MODULE);
        let provider = provider();
        let limits = limits(4096);
        let request = fixture.request(&limits, language, Vec::new());
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        assert!(required > 1);
        let output = parse(&provider, &request);
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            output.report().coverage().covered_source_bytes(),
            CLOSED_MODULE.len()
        );
        assert!(output.diagnostics().is_empty());
        for name in [b"Run".as_slice(), b"Count"] {
            assert!(output.facts().iter().any(|fact| {
                fact.syntax_kind().as_str().ends_with(".definition")
                    && bytes_at(CLOSED_MODULE, fact.span()) == name
            }));
        }
        for fact in output.facts() {
            assert_eq!(fact.span().file(), fixture.source.span().file());
            assert!(fact.span().end_byte() <= fixture.source.span().end_byte());
            if let Some(parent) = fact.parent() {
                assert!(
                    output
                        .facts()
                        .iter()
                        .any(|candidate| candidate.local_id() == parent)
                );
            }
        }
        let repeated = parse(&provider, &request);
        assert_eq!(output.facts(), repeated.facts());
        assert_eq!(output.report(), repeated.report());
        let stats = provider.stats();
        assert_eq!(stats.pooled_parsers, 0);
        assert_eq!(stats.checked_out_parsers, 0);
        assert_eq!(stats.cache.entries, 0);
    }
}

#[test]
fn unsupported_tail_is_not_hidden_by_a_valid_prefix_or_included_range() {
    let source =
        b"Sub Empty()\nEnd Sub\nSub Populated(ByVal value As Long)\nvalue = value + 1\nEnd Sub\n";
    for scoped in [false, true] {
        let fixture = Fixture::new(source);
        let provider = provider();
        let limits = limits(4096);
        let ranges = if scoped {
            vec![IncludedRange::new(
                SourceSpan::new(fixture.source.span().file(), 0, 19).unwrap(),
                LanguageId::new("vba").unwrap(),
            )]
        } else {
            Vec::new()
        };
        let request = fixture.request(&limits, "vba", ranges);
        let _required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        let output = parse(&provider, &request);
        assert!(provider.stats().pooled_parsers > 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert!(output.facts().iter().any(|fact| {
            fact.syntax_kind().as_str().ends_with(".definition")
                && bytes_at(source, fact.span()) == b"Empty"
        }));
        if !scoped {
            assert!(output.facts().iter().any(|fact| {
                fact.syntax_kind().as_str().ends_with(".definition")
                    && bytes_at(source, fact.span()) == b"Populated"
            }));
        }
    }
}

#[test]
fn native_object_context_cannot_be_promoted_to_a_procedural_file() {
    let fixture = Fixture::new(CLOSED_MODULE);
    let provider = provider();
    let limits = limits(4096);
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        LanguageId::new("vba").unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(rootlight_ids::content_hash(b"authored-context")),
        &limits,
    )
    .unwrap()
    .with_native_vba_module(NativeVbaModuleKind::Object)
    .unwrap()
    .to_parse_request();
    let output = parse(&provider, &request);
    assert!(provider.stats().pooled_parsers > 0);
    assert!(
        output
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "vba.class.module")
    );
    assert!(
        !output
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "vba.file.module")
    );
}

#[test]
fn native_continuation_is_invalidated_when_an_edit_enters_the_first_party_route() {
    let native = b"Sub Run()\nDim value As Long\nEnd Sub\n";
    let first_party = b"Sub Run()\nEnd Sub\n";
    let mut fixture = Fixture::new(native);
    let provider = provider();
    let limits = limits(4096);
    let settings = ParserSettings::new(512).unwrap();
    let request = fixture.request(&limits, "vba", Vec::new());
    let previous = provider
        .execute_with_previous(
            &request,
            None,
            &[],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let continuation = previous
        .previous()
        .expect("native parse owns a real cached tree");
    let old_stats = provider.stats();
    fixture.rewrite(first_party);
    let request = fixture.request(&limits, "vba", Vec::new());
    let edit = SourceEdit::new(10, 28, "").unwrap();
    let output = provider
        .execute_with_previous(
            &request,
            Some(continuation),
            &[edit],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        output.reuse_status(),
        ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
    );
    assert_eq!(
        output.reuse_key().grammar_version(),
        VBA_PROCEDURAL_FRONTEND_VERSION
    );
    assert!(output.previous().is_none());
    assert_eq!(output.output().facts(), parse(&provider, &request).facts());
    assert_eq!(provider.stats().cache.entries, old_stats.cache.entries);
    assert_eq!(provider.stats().pooled_parsers, old_stats.pooled_parsers);
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn first_party_identity_preflight_matches_exact_fit_and_one_short_fact_admission() {
    let fixture = Fixture::new(CLOSED_MODULE);
    let provider = provider();
    let full_limits = limits(4096);
    let request = fixture.request(&full_limits, "vba", Vec::new());
    let required = provider
        .required_syntax_fact_count(&request, &deadline())
        .unwrap();
    assert!(required > 1);
    let exact_limits = limits(required);
    let request = fixture.request(&exact_limits, "vba", Vec::new());
    let output = parse(&provider, &request);
    for name in [b"Run".as_slice(), b"Count"] {
        assert!(output.facts().iter().any(|fact| {
            fact.syntax_kind().as_str().ends_with(".definition")
                && bytes_at(CLOSED_MODULE, fact.span()) == name
        }));
    }
    let short_limits = limits(required - 1);
    let request = fixture.request(&short_limits, "vba", Vec::new());
    assert_eq!(
        execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1,
        })
    );
    assert_eq!(provider.stats().pooled_parsers, 0);
}

#[test]
fn cancellation_does_not_trigger_native_fallback_or_leave_output_state() {
    let fixture = Fixture::new(CLOSED_MODULE);
    let provider = provider();
    let limits = limits(4096);
    let request = fixture.request(&limits, "vba", Vec::new());
    let cancelled = deadline();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled,
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert_eq!(provider.stats().pooled_parsers, 0);
    assert_eq!(provider.stats().cache.entries, 0);
    assert!(!parse(&provider, &request).facts().is_empty());
}

fn bytes_at(source: &[u8], span: SourceSpan) -> &[u8] {
    source
        .get(usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap())
        .unwrap()
}

fn parse(provider: &TreeSitterProvider, request: &ParseRequest<'_>) -> ParseOutput {
    execute_parse(
        provider,
        request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("bounded parse commits")
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn provider() -> TreeSitterProvider {
    TreeSitterProvider::new(
        RuntimeConfig::new(
            SOURCE_LIMIT,
            4096,
            128,
            32,
            64,
            1,
            4 * 1024 * 1024,
            ParserSettings::new(512).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn limits(records: usize) -> AnalysisLimits {
    let batch = BatchThresholds::new(records.min(128), 128 * 1024, 4, 4096).unwrap();
    let syntax = StreamLimits::new(
        64,
        records,
        4 * 1024 * 1024,
        16,
        64 * 1024,
        128 * 1024,
        batch,
    )
    .unwrap();
    let ir = StreamLimits::new(
        8192,
        4096,
        16 * 1024 * 1024,
        128,
        1024 * 1024,
        4 * 1024 * 1024,
        BatchThresholds::new(128, 1024 * 1024, 16, 64 * 1024).unwrap(),
    )
    .unwrap();
    AnalysisLimits::new(
        SOURCE_LIMIT,
        4096,
        128,
        32,
        8 * 1024 * 1024,
        syntax,
        ir,
        IrLimits::default(),
    )
    .unwrap()
}

struct Fixture {
    _temporary: TempDir,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let temporary = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let (snapshot, source) = capture(&temporary, bytes);
        Self {
            _temporary: temporary,
            snapshot,
            source,
        }
    }

    fn rewrite(&mut self, bytes: &[u8]) {
        let (snapshot, source) = capture(&self._temporary, bytes);
        self.snapshot = snapshot;
        self.source = source;
    }

    fn request<'a>(
        &'a self,
        limits: &'a AnalysisLimits,
        language: &str,
        ranges: Vec<IncludedRange>,
    ) -> ParseRequest<'a> {
        ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            LanguageId::new(language).unwrap(),
            EncodingId::new("utf-8").unwrap(),
            ranges,
            limits,
        )
        .unwrap()
    }
}

fn capture(temporary: &TempDir, bytes: &[u8]) -> (SourceSnapshot, SourceRef) {
    fs::write(temporary.path().join("module.bas"), bytes).unwrap();
    let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
    let repository = RepositoryRoot::open(repository_id, temporary.path()).unwrap();
    let snapshot = repository
        .snapshot(
            &RelativePath::parse(Path::new("module.bas")).unwrap(),
            u64::try_from(SOURCE_LIMIT).unwrap(),
        )
        .unwrap();
    let source = SourceRef::new(
        repository_id,
        "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
            .parse()
            .unwrap(),
        SourceSpan::new(
            snapshot.file(),
            0,
            u64::try_from(snapshot.content().len()).unwrap(),
        )
        .unwrap(),
        snapshot.content_hash(),
        None,
    );
    (snapshot, source)
}
