//! Original-byte contracts for encoded native parsing through the public SDK.
//! Query predicates and embedded grammars must share a decoded text view while
//! committed facts, reports and reuse identities retain the immutable snapshot.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, BatchThresholds, EncodingId, GenerationBoundSnapshot,
    IncludedRange, LanguageId, MemoryAdmissionPolicy, ParseOutput, ParseRequest, ResourceKind,
    SinkError, StreamLimits, SyntaxFactKind, execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, ReuseInvalidation, ReuseStatus, RuntimeConfig, SourceEdit, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::content_hash;
use rootlight_ir::{CoverageStatus, IrLimits, SourceRef, SourceSpan, SourceTextEncoding};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const ENCODINGS: [SourceTextEncoding; 2] =
    [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be];
const MEMORY_POLICY: MemoryAdmissionPolicy =
    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;

#[test]
fn encoded_native_predicates_and_captures_keep_exact_physical_spans() {
    let text = "// 𝄞\r\n/** docs */\r\nclass Demo { @Test void α() {} @Other void other() {} }\r\n";
    let budget = limits();
    let provider = provider();
    let control = Fixture::new("Sample.java", text.as_bytes());
    let control_request = request(&control, &budget, "java", "utf-8", Vec::new());
    let control_output = parse(&provider, &control_request);
    assert_eq!(
        control_output.report().coverage().status(),
        CoverageStatus::Complete
    );

    for encoding in ENCODINGS {
        let raw = encode(text, encoding);
        let fixture = Fixture::new("Sample.java", &raw);
        let request = request(&fixture, &budget, "java", encoding.as_str(), Vec::new());
        let output = parse(&provider, &request);
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(output.diagnostics().is_empty());
        assert_physical_facts(&output, &fixture, encoding);
        assert_same_projected_captures(&output, &control_output, text);
        assert_capture(&output, &fixture, text, "java.identifier.definition", "α");

        let test_attributes: Vec<_> = output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str().ends_with(".test_attribute"))
            .map(|fact| decode_span(&fixture, fact.span(), encoding))
            .collect();
        assert_eq!(test_attributes, ["Test"]);
        let documentation: Vec<_> = output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str().ends_with(".documentation"))
            .map(|fact| decode_span(&fixture, fact.span(), encoding))
            .collect();
        assert_eq!(documentation, ["/** docs */"]);
        assert_eq!(fixture.snapshot.content(), raw);
        assert_eq!(
            request.source().source_ref().content_hash(),
            content_hash(&raw)
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn encoded_disjoint_ranges_conserve_original_bytes_without_gap_captures() {
    let text = "𝄞 ignored\r\nfn first() {}\r\nnot rust at all\r\nfn second() {}\r\n";
    let budget = limits();
    let provider = provider();
    for encoding in ENCODINGS {
        let fixture = Fixture::new("ranges.rs", &encode(text, encoding));
        let included: Vec<_> = ["fn first() {}\r\n", "fn second() {}\r\n"]
            .into_iter()
            .map(|body| {
                IncludedRange::new(
                    physical_span(&fixture, text, body),
                    LanguageId::new("rust").unwrap(),
                )
            })
            .collect();
        let expected_covered: usize = included
            .iter()
            .map(|range| {
                usize::try_from(range.span().end_byte() - range.span().start_byte()).unwrap()
            })
            .sum();
        let request = request(
            &fixture,
            &budget,
            "rust",
            encoding.as_str(),
            included.clone(),
        );
        let output = parse(&provider, &request);
        assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
        assert_eq!(
            output.report().coverage().covered_source_bytes(),
            expected_covered
        );
        assert_eq!(
            output.report().coverage().total_source_bytes(),
            fixture.snapshot.content().len()
        );
        assert_physical_facts(&output, &fixture, encoding);
        assert!(
            output
                .facts()
                .iter()
                .all(|fact| included.iter().any(|range| {
                    range.span().start_byte() <= fact.span().start_byte()
                        && fact.span().end_byte() <= range.span().end_byte()
                }))
        );
        assert_eq!(
            output
                .facts()
                .iter()
                .filter(|fact| fact.kind() == SyntaxFactKind::Root)
                .count(),
            2
        );
        assert_capture(
            &output,
            &fixture,
            text,
            "rust.identifier.definition",
            "first",
        );
        assert_capture(
            &output,
            &fixture,
            text,
            "rust.identifier.definition",
            "second",
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn encoded_embedded_grammars_preserve_whole_host_coordinates() {
    let text = "<main>𝄞é\r\n<script>function first(value) { return value; }</script><style>.item { --tone: red; }</style><script>function second(value) { return value; }</script></main>";
    let budget = limits();
    let provider = provider();
    let control = Fixture::new("view.html", text.as_bytes());
    let control_output = parse(
        &provider,
        &request(&control, &budget, "html", "utf-8", Vec::new()),
    );
    assert!(control_output.diagnostics().is_empty());
    for encoding in ENCODINGS {
        let fixture = Fixture::new("view.html", &encode(text, encoding));
        let request = request(&fixture, &budget, "html", encoding.as_str(), Vec::new());
        let output = parse(&provider, &request);
        assert!(output.diagnostics().is_empty());
        assert_physical_facts(&output, &fixture, encoding);
        assert_same_projected_captures(&output, &control_output, text);
        assert_capture(
            &output,
            &fixture,
            text,
            "javascript.identifier.definition",
            "first",
        );
        assert_capture(
            &output,
            &fixture,
            text,
            "javascript.identifier.definition",
            "second",
        );
        assert!(output.facts().iter().any(|fact| {
            fact.syntax_kind().as_str().starts_with("css.")
                && fact.span() == physical_span(&fixture, text, "--tone")
        }));
        assert_eq!(
            output.report().coverage().total_source_bytes(),
            fixture.snapshot.content().len()
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn encoded_same_source_reuses_but_utf8_edits_cold_invalidate() {
    let before = "// 𝄞\r\nfn first() {}\r\n";
    let after = "// 𝄞\r\nfn second() {}\r\n";
    let budget = limits();
    let settings = ParserSettings::new(128).unwrap();
    for encoding in ENCODINGS {
        let provider = provider();
        let fixture = Fixture::new("reuse.rs", &encode(before, encoding));
        let original = request(&fixture, &budget, "rust", encoding.as_str(), Vec::new());
        let first = provider
            .execute_with_previous(&original, None, &[], settings, MEMORY_POLICY, &deadline())
            .expect("encoded initial parse commits");
        assert_eq!(first.reuse_status(), ReuseStatus::Fresh);
        let handle = first
            .previous()
            .expect("bounded fixture retains its native tree");
        let unchanged = provider
            .execute_with_previous(
                &original,
                Some(handle),
                &[],
                settings,
                MEMORY_POLICY,
                &deadline(),
            )
            .expect("identical encoded source reuses without UTF8 edit validation");
        assert_eq!(
            unchanged.reuse_status(),
            ReuseStatus::Reused { changed_ranges: 0 }
        );
        assert_eq!(first.output(), unchanged.output());
        assert_eq!(
            unchanged.reuse_key().current_content_hash(),
            fixture.snapshot.content_hash()
        );
        assert_eq!(unchanged.reuse_key().encoding(), encoding.as_str());

        let changed = fixture.rewrite(&encode(after, encoding));
        let changed_request = request(&changed, &budget, "rust", encoding.as_str(), Vec::new());
        let old_name = physical_span(&fixture, before, "first");
        let edit = SourceEdit::new(
            usize::try_from(old_name.start_byte()).unwrap(),
            usize::try_from(old_name.end_byte()).unwrap(),
            "second",
        )
        .unwrap();
        let rebuilt = provider
            .execute_with_previous(
                &changed_request,
                unchanged.previous(),
                &[edit],
                settings,
                MEMORY_POLICY,
                &deadline(),
            )
            .expect("UTF8 edit metadata cannot poison an encoded cold parse");
        assert_eq!(
            rebuilt.reuse_status(),
            ReuseStatus::Invalidated(ReuseInvalidation::Encoding)
        );
        let clean = provider
            .execute_with_previous(
                &changed_request,
                None,
                &[],
                settings,
                MEMORY_POLICY,
                &deadline(),
            )
            .expect("encoded clean control commits");
        assert_eq!(rebuilt.output(), clean.output());
        assert_physical_facts(rebuilt.output(), &changed, encoding);
        assert_capture(
            rebuilt.output(),
            &changed,
            after,
            "rust.identifier.definition",
            "second",
        );
        assert_eq!(
            rebuilt.reuse_key().previous_content_hash(),
            Some(fixture.snapshot.content_hash())
        );
        assert_eq!(
            rebuilt.reuse_key().current_content_hash(),
            changed.snapshot.content_hash()
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn encoded_malformed_bodies_and_split_scalar_ranges_fail_before_admission() {
    let budget = limits();
    let provider = provider();
    for encoding in ENCODINGS {
        let mut malformed = encode("fn sample() {}\r\n", encoding);
        let unit = match encoding {
            SourceTextEncoding::Utf16Le => 0xd800_u16.to_le_bytes(),
            SourceTextEncoding::Utf16Be => 0xd800_u16.to_be_bytes(),
        };
        malformed.extend_from_slice(&unit);
        let mut odd = encode("fn sample() {}\r\n", encoding);
        odd.push(1);
        let mut nul = encode("fn sample() {}\r\n", encoding);
        nul.extend_from_slice(&[0, 0]);
        let mut mismatched_bom = encode("fn sample() {}\r\n", encoding);
        mismatched_bom.swap(0, 1);
        let mut with_bom = encode("fn sample() {}\r\n", encoding);
        let bomless = with_bom.split_off(2);
        for raw in [malformed, odd, nul, mismatched_bom, bomless] {
            let fixture = Fixture::new("invalid.rs", &raw);
            let request = request(&fixture, &budget, "rust", encoding.as_str(), Vec::new());
            assert!(
                matches!(execute_parse(&provider, &request, MEMORY_POLICY, &deadline()),
                Err(AdapterError::ProviderFailed { code }) if code.as_str() == "native-source-invalid-utf16")
            );
            assert_eq!(provider.stats().checked_out_parsers, 0);
        }
        let text = "// 𝄞\r\nfn sample() {}\r\n";
        let fixture = Fixture::new("split.rs", &encode(text, encoding));
        let scalar = physical_span(&fixture, text, "𝄞");
        let split = IncludedRange::new(
            SourceSpan::new(
                fixture.snapshot.file(),
                scalar.start_byte() + 2,
                fixture.source.span().end_byte(),
            )
            .unwrap(),
            LanguageId::new("rust").unwrap(),
        );
        let request = request(&fixture, &budget, "rust", encoding.as_str(), vec![split]);
        assert!(
            matches!(execute_parse(&provider, &request, MEMORY_POLICY, &deadline()),
            Err(AdapterError::ProviderFailed { code }) if code.as_str() == "native-source-boundary")
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn encoded_recovery_diagnostics_keep_original_source_identity() {
    let text = "// 𝄞\r\nfn sample( {\r\n";
    let budget = limits();
    let provider = provider();
    let control = Fixture::new("recovery.rs", text.as_bytes());
    let control_output = parse(
        &provider,
        &request(&control, &budget, "rust", "utf-8", Vec::new()),
    );
    let control_recovery = control_output
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.code().as_str() == "syntax-error-recovery")
        .unwrap()
        .source()
        .unwrap()
        .span();
    for encoding in ENCODINGS {
        let fixture = Fixture::new("recovery.rs", &encode(text, encoding));
        let output = parse(
            &provider,
            &request(&fixture, &budget, "rust", encoding.as_str(), Vec::new()),
        );
        assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
        assert_physical_facts(&output, &fixture, encoding);
        let recovery = output
            .diagnostics()
            .iter()
            .find(|diagnostic| diagnostic.code().as_str() == "syntax-error-recovery")
            .unwrap()
            .source()
            .unwrap();
        assert_eq!(
            usize::try_from(recovery.span().start_byte()).unwrap(),
            physical_offset(
                text,
                usize::try_from(control_recovery.start_byte()).unwrap()
            )
        );
        assert_eq!(
            usize::try_from(recovery.span().end_byte()).unwrap(),
            physical_offset(text, usize::try_from(control_recovery.end_byte()).unwrap())
        );
        assert_eq!(recovery.content_hash(), fixture.snapshot.content_hash());
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn encoded_projection_obeys_existing_source_and_owned_memory_limits() {
    let text = format!("// {}\r\nfn sample() {{}}\r\n", "界".repeat(32));
    let provider = provider();
    for encoding in ENCODINGS {
        let fixture = Fixture::new("budget.rs", &encode(&text, encoding));
        let ordinary = limits();
        for (source_limit, memory_limit, expected) in [
            (
                fixture.snapshot.content().len(),
                ordinary.max_reported_memory_bytes(),
                ResourceKind::SourceBytes,
            ),
            (MAX_SOURCE_BYTES, 1, ResourceKind::ReportedMemoryBytes),
        ] {
            let budget = AnalysisLimits::new(
                source_limit,
                ordinary.max_syntax_nodes(),
                ordinary.max_syntax_depth(),
                ordinary.max_embedded_ranges(),
                memory_limit,
                ordinary.syntax_stream().clone(),
                ordinary.ir_stream().clone(),
                ordinary.ir().clone(),
            )
            .unwrap();
            let request = request(&fixture, &budget, "rust", encoding.as_str(), Vec::new());
            assert!(
                matches!(execute_parse(&provider, &request, MEMORY_POLICY, &deadline()),
                Err(AdapterError::Sink(SinkError::StreamLimit { resource, .. })) if resource == expected)
            );
            assert_eq!(provider.stats().checked_out_parsers, 0);
        }
    }
}

fn assert_same_projected_captures(encoded: &ParseOutput, utf8: &ParseOutput, text: &str) {
    let mut expected: Vec<_> = utf8
        .facts()
        .iter()
        .filter(|fact| fact.kind() != SyntaxFactKind::Root)
        .map(|fact| {
            (
                fact.syntax_kind().as_str(),
                physical_offset(text, usize::try_from(fact.span().start_byte()).unwrap()),
                physical_offset(text, usize::try_from(fact.span().end_byte()).unwrap()),
            )
        })
        .collect();
    let mut actual: Vec<_> = encoded
        .facts()
        .iter()
        .filter(|fact| fact.kind() != SyntaxFactKind::Root)
        .map(|fact| {
            (
                fact.syntax_kind().as_str(),
                usize::try_from(fact.span().start_byte()).unwrap(),
                usize::try_from(fact.span().end_byte()).unwrap(),
            )
        })
        .collect();
    expected.sort_unstable();
    actual.sort_unstable();
    assert_eq!(actual, expected);
}

fn assert_physical_facts(output: &ParseOutput, fixture: &Fixture, encoding: SourceTextEncoding) {
    for fact in output.facts() {
        let span = fact.span();
        assert_eq!(span.file(), fixture.snapshot.file());
        assert!(span.start_byte() <= span.end_byte());
        assert!(span.end_byte() <= fixture.source.span().end_byte());
        assert_eq!(span.start_byte() % 2, 0);
        assert_eq!(span.end_byte() % 2, 0);
        let _ = decode_span(fixture, span, encoding);
        if let Some(parent) = fact.parent() {
            let parent = output
                .facts()
                .iter()
                .find(|candidate| candidate.local_id() == parent)
                .unwrap();
            assert!(parent.span().start_byte() <= span.start_byte());
            assert!(span.end_byte() <= parent.span().end_byte());
        }
    }
    for diagnostic in output.diagnostics() {
        if let Some(source) = diagnostic.source() {
            assert_eq!(source.repository(), fixture.source.repository());
            assert_eq!(source.generation(), fixture.source.generation());
            assert_eq!(source.content_hash(), fixture.snapshot.content_hash());
            let _ = decode_span(fixture, source.span(), encoding);
        }
    }
}

fn assert_capture(
    output: &ParseOutput,
    fixture: &Fixture,
    text: &str,
    label: &str,
    spelling: &str,
) {
    let expected = physical_span(fixture, text, spelling);
    assert!(
        output
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == label && fact.span() == expected)
    );
}

fn physical_span(fixture: &Fixture, text: &str, spelling: &str) -> SourceSpan {
    let start = text.find(spelling).unwrap();
    SourceSpan::new(
        fixture.snapshot.file(),
        u64::try_from(physical_offset(text, start)).unwrap(),
        u64::try_from(physical_offset(text, start + spelling.len())).unwrap(),
    )
    .unwrap()
}

fn physical_offset(text: &str, offset: usize) -> usize {
    2 + text.get(..offset).unwrap().encode_utf16().count() * 2
}

fn decode_span(fixture: &Fixture, span: SourceSpan, encoding: SourceTextEncoding) -> String {
    let raw = fixture
        .snapshot
        .content()
        .get(usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap())
        .unwrap();
    assert_eq!(raw.len() % 2, 0);
    let units: Vec<_> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| match encoding {
            SourceTextEncoding::Utf16Le => u16::from_le_bytes([pair[0], pair[1]]),
            SourceTextEncoding::Utf16Be => u16::from_be_bytes([pair[0], pair[1]]),
        })
        .collect();
    String::from_utf16(&units).expect("physical evidence never splits a surrogate pair")
}

fn encode(text: &str, encoding: SourceTextEncoding) -> Vec<u8> {
    let mut bytes = match encoding {
        SourceTextEncoding::Utf16Le => vec![0xff, 0xfe],
        SourceTextEncoding::Utf16Be => vec![0xfe, 0xff],
    };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&match encoding {
            SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
            SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
        });
    }
    bytes
}

fn parse(provider: &TreeSitterProvider, request: &ParseRequest<'_>) -> ParseOutput {
    execute_parse(provider, request, MEMORY_POLICY, &deadline())
        .expect("encoded native parser commits")
}

fn provider() -> TreeSitterProvider {
    TreeSitterProvider::new(
        RuntimeConfig::new(
            MAX_SOURCE_BYTES,
            16_384,
            128,
            16,
            64,
            1,
            2 * MAX_SOURCE_BYTES,
            ParserSettings::new(128).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn limits() -> AnalysisLimits {
    let batch = BatchThresholds::new(128, 256 * 1024, 8, 4096).unwrap();
    let stream =
        StreamLimits::new(64, 4096, 4 * 1024 * 1024, 64, 64 * 1024, 128 * 1024, batch).unwrap();
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        16_384,
        128,
        16,
        8 * MAX_SOURCE_BYTES,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
}

fn request<'a>(
    fixture: &'a Fixture,
    budget: &'a AnalysisLimits,
    language: &str,
    encoding: &str,
    ranges: Vec<IncludedRange>,
) -> ParseRequest<'a> {
    ParseRequest::new(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        LanguageId::new(language).unwrap(),
        EncodingId::new(encoding).unwrap(),
        ranges,
        budget,
    )
    .unwrap()
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

struct Fixture {
    temporary: Arc<TempDir>,
    relative: RelativePath,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(name: &str, bytes: &[u8]) -> Self {
        let temporary = Arc::new(tempdir_in(std::env::current_dir().unwrap()).unwrap());
        let relative = RelativePath::parse(Path::new(name)).unwrap();
        fs::write(temporary.path().join(name), bytes).unwrap();
        Self::capture(temporary, relative)
    }

    fn rewrite(&self, bytes: &[u8]) -> Self {
        fs::write(self.temporary.path().join(self.relative.as_str()), bytes).unwrap();
        Self::capture(Arc::clone(&self.temporary), self.relative.clone())
    }

    fn capture(temporary: Arc<TempDir>, relative: RelativePath) -> Self {
        let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
        let repository = RepositoryRoot::open(repository_id, temporary.path()).unwrap();
        let snapshot = repository
            .snapshot(&relative, u64::try_from(MAX_SOURCE_BYTES).unwrap())
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
        Self {
            temporary,
            relative,
            snapshot,
            source,
        }
    }
}
