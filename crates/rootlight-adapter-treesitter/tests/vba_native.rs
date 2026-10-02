//! Public SDK contracts for native VBA parsing, bounded output, and tree reuse.
//! Source-role checks complement analyzer relationship tests without exposing
//! Tree-sitter values through the SDK.

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, NativeVbaModuleKind, ParseOutput,
    ParseRequest, ResourceKind, SinkError, StreamLimits, SyntaxFact, SyntaxFactKind, execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, ReuseStatus, RuntimeConfig, SourceEdit, TreeSitterProvider,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, IrLimits, SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const SOURCE_LIMIT: usize = 1024 * 1024;
const MODULE: &[u8] = b"Attribute VB_Name = \"Authored\"\nSub Run(ByVal value As Long)\nDim sink As Variant\nsink = #1/1/2000#\nEnd Sub\nFunction Echo(item As Long) As Long\nEcho = item\nEnd Function\n";

#[test]
fn procedural_modules_use_native_cached_trees_across_edits() {
    for language in ["vba", "visual-basic-classic"] {
        let mut fixture = Fixture::new(MODULE);
        let provider = native_provider();
        let budget = limits(4096);
        let settings = ParserSettings::new(512).unwrap();
        let request = fixture.request(&budget, language);
        let initial = provider
            .execute_with_previous(
                &request,
                None,
                &[],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(initial.reuse_status(), ReuseStatus::Fresh);
        assert_eq!(initial.reuse_key().grammar_version(), "0.14.1");
        assert_complete(initial.output(), &fixture);
        let previous = initial
            .previous()
            .expect("native tree fits the cache")
            .clone();
        assert!(provider.stats().pooled_parsers > 0);
        assert!(provider.stats().cache.entries > 0);
        let repeated = provider
            .execute_with_previous(
                &request,
                Some(&previous),
                &[],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert!(matches!(
            repeated.reuse_status(),
            ReuseStatus::Reused { .. }
        ));
        assert_eq!(initial.output().facts(), repeated.output().facts());
        let previous = repeated.previous().unwrap().clone();
        let old_hash = fixture.source.content_hash();
        let source = std::str::from_utf8(MODULE).unwrap();
        let start = source.find("#1/1/2000#").unwrap();
        let replacement = "(value + 1) * -2";
        let changed = source.replace("#1/1/2000#", replacement);
        fixture.rewrite(changed.as_bytes());
        let request = fixture.request(&budget, language);
        let edited = provider
            .execute_with_previous(
                &request,
                Some(&previous),
                &[SourceEdit::new(start, start + "#1/1/2000#".len(), replacement).unwrap()],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert!(matches!(edited.reuse_status(), ReuseStatus::Reused { .. }));
        assert!(edited.previous().is_some());
        assert_eq!(edited.reuse_key().grammar_version(), "0.14.1");
        assert_eq!(edited.reuse_key().previous_content_hash(), Some(old_hash));
        assert_eq!(
            edited.reuse_key().current_content_hash(),
            fixture.source.content_hash()
        );
        assert_complete(edited.output(), &fixture);
        let clean = parse(&native_provider(), &request);
        assert_eq!(edited.output().facts(), clean.facts());
        assert_eq!(edited.output().diagnostics(), clean.diagnostics());
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn native_preflight_preserves_identity_at_exact_and_short_fact_budgets() {
    for language in ["vba", "visual-basic-classic"] {
        let fixture = Fixture::new(MODULE);
        let provider = native_provider();
        let full_limits = limits(4096);
        let request = fixture.request(&full_limits, language);
        let required = provider
            .required_syntax_fact_count(&request, &deadline())
            .unwrap();
        let full = parse(&provider, &request);
        assert_complete(&full, &fixture);
        assert!(required > 1 && required < full.facts().len());
        assert!(provider.stats().pooled_parsers > 0);
        let exact_limits = limits(required);
        let exact = parse(&provider, &fixture.request(&exact_limits, language));
        assert_eq!(exact.facts().len(), required);
        assert_eq!(exact.report().coverage().status(), CoverageStatus::Bounded);
        assert!(!exact.diagnostics().is_empty());
        for declaration in full
            .facts()
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::Declaration)
        {
            assert!(
                exact
                    .facts()
                    .iter()
                    .any(|fact| same_source_fact(fact, declaration))
            );
        }
        for fact in exact.facts() {
            let original = full
                .facts()
                .iter()
                .find(|candidate| same_source_fact(fact, candidate))
                .unwrap();
            let parent = fact.parent().map(|id| {
                exact
                    .facts()
                    .iter()
                    .find(|candidate| candidate.local_id() == id)
                    .unwrap()
            });
            let original_parent = original.parent().map(|id| {
                full.facts()
                    .iter()
                    .find(|candidate| candidate.local_id() == id)
                    .unwrap()
            });
            assert_eq!(parent.is_some(), original_parent.is_some());
            if let (Some(parent), Some(original_parent)) = (parent, original_parent) {
                assert!(same_source_fact(parent, original_parent));
            }
        }
        let short_limits = limits(required - 1);
        assert_eq!(
            execute_parse(
                &provider,
                &fixture.request(&short_limits, language),
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
        let cancellation = deadline();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancellation,
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(full.facts(), parse(&provider, &request).facts());
    }
}

#[test]
fn native_grouped_statement_calls_keep_exact_roles_and_literal_spans() {
    for newline in ["\n", "\r\n"] {
        for statement in [
            "Target(Nothing)",
            "Target (Nothing)",
            "[Target]((value))",
            "Target(Inner(value, other))",
        ] {
            let source = format!(
                "Sub Run(ByVal value As Long, ByVal other As Long){newline}Dim sink As Variant{newline}sink = Target(value){newline}Set sink = Target(value){newline}{statement} ' grouped statement{newline}End Sub{newline}"
            );
            for language in ["vba", "visual-basic-classic"] {
                let fixture = Fixture::new(source.as_bytes());
                let provider = native_provider();
                let budget = limits(4096);
                let output = parse(&provider, &fixture.request(&budget, language));
                assert_complete(&output, &fixture);
                let start = source
                    .find(&format!("{statement} ' grouped statement"))
                    .unwrap();
                let callee = if statement.starts_with('[') {
                    "[Target]"
                } else {
                    "Target"
                };
                assert_role(
                    &output,
                    start,
                    callee.len(),
                    "vba.invoked_identifier.reference",
                    "vba.invoked_get_identifier.reference",
                );
                assert_eq!(
                    output
                        .facts()
                        .iter()
                        .filter(|fact| {
                            fact.syntax_kind().as_str() == "vba.application.reference"
                                && fact.span().start_byte() == u64::try_from(start).unwrap()
                                && fact.span().end_byte()
                                    == u64::try_from(start + statement.len()).unwrap()
                        })
                        .count(),
                    1
                );
                for assignment in ["sink = Target(value)", "Set sink = Target(value)"] {
                    let start =
                        source.find(assignment).unwrap() + assignment.find("Target").unwrap();
                    assert_role(
                        &output,
                        start,
                        "Target".len(),
                        "vba.invoked_get_identifier.reference",
                        "vba.invoked_identifier.reference",
                    );
                }
                if let Some(offset) = statement.find("Nothing") {
                    let start = u64::try_from(start + offset).unwrap();
                    assert!(!output.facts().iter().any(|fact| {
                        matches!(
                            fact.kind(),
                            SyntaxFactKind::Occurrence | SyntaxFactKind::StringLiteral
                        ) && fact.span().start_byte() >= start
                            && fact.span().end_byte() <= start + 7
                    }));
                }
            }
        }
    }
}

#[test]
fn native_object_metadata_cannot_be_promoted_to_a_procedural_file() {
    let fixture = Fixture::new(MODULE);
    let provider = native_provider();
    let budget = limits(4096);
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        LanguageId::new("vba").unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(rootlight_ids::content_hash(b"authored-context")),
        &budget,
    )
    .unwrap()
    .with_native_vba_module(NativeVbaModuleKind::Object)
    .unwrap()
    .to_parse_request();
    let output = parse(&provider, &request);
    assert_complete(&output, &fixture);
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
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

fn assert_role(output: &ParseOutput, start: usize, length: usize, expected: &str, excluded: &str) {
    let span = |fact: &&SyntaxFact| {
        fact.span().start_byte() == u64::try_from(start).unwrap()
            && fact.span().end_byte() == u64::try_from(start + length).unwrap()
    };
    assert_eq!(
        output
            .facts()
            .iter()
            .filter(span)
            .filter(|fact| fact.syntax_kind().as_str() == expected)
            .count(),
        1
    );
    assert!(
        !output
            .facts()
            .iter()
            .filter(span)
            .any(|fact| fact.syntax_kind().as_str() == excluded)
    );
}

fn same_source_fact(left: &SyntaxFact, right: &SyntaxFact) -> bool {
    left.kind() == right.kind()
        && left.span() == right.span()
        && left.syntax_kind() == right.syntax_kind()
}

fn assert_complete(output: &ParseOutput, fixture: &Fixture) {
    assert_eq!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        output.report().coverage().covered_source_bytes(),
        fixture.snapshot.content().len()
    );
    assert!(output.report().resources().syntax_nodes() > 0);
    assert!(output.diagnostics().is_empty());
    assert_eq!(
        fixture.source.content_hash(),
        rootlight_ids::content_hash(fixture.snapshot.content())
    );
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
}

fn parse(provider: &TreeSitterProvider, request: &ParseRequest<'_>) -> ParseOutput {
    execute_parse(
        provider,
        request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap()
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn native_provider() -> TreeSitterProvider {
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
    let syntax = StreamLimits::new(
        64,
        records,
        4 * 1024 * 1024,
        16,
        64 * 1024,
        128 * 1024,
        BatchThresholds::new(records.min(128), 128 * 1024, 4, 4096).unwrap(),
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
    temporary: TempDir,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let temporary = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let (snapshot, source) = capture(&temporary, bytes);
        Self {
            temporary,
            snapshot,
            source,
        }
    }

    fn rewrite(&mut self, bytes: &[u8]) {
        (self.snapshot, self.source) = capture(&self.temporary, bytes);
    }

    fn request<'a>(&'a self, limits: &'a AnalysisLimits, language: &str) -> ParseRequest<'a> {
        ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            LanguageId::new(language).unwrap(),
            EncodingId::new("utf-8").unwrap(),
            Vec::new(),
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
