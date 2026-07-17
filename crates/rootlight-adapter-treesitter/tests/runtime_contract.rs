//! Public contract tests for bounded parsing and provider-owned incremental reuse.
//!
//! Fixtures use real VFS snapshots so source identity, included ranges, cache
//! eviction, malformed input, and cancellation cross the complete SDK boundary.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, BatchThresholds, EncodingId, GenerationBoundSnapshot,
    IncludedRange, LanguageId, MemoryAdmissionPolicy, MemoryAdmissionStatus, ParseRequest,
    RequestError, StreamLimits, execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, ReuseInvalidation, ReuseStatus, RuntimeConfig, SourceEdit, TreeSitterProvider,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::content_hash;
use rootlight_ir::{CoverageStatus, IrLimits, SourceRef, SourceSpan};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

#[test]
fn incremental_executor_enforces_deadline_and_explicit_memory_admission() {
    let fixture = Fixture::new("admission.rs", b"fn admitted() {}\n");
    let limits = limits(MAX_SOURCE_BYTES, 1024, 64);
    let provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    let settings = ParserSettings::new(64).expect("settings are bounded");
    let request = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "rust",
        Vec::new(),
    );

    assert!(matches!(
        provider.execute_with_previous(
            &request,
            None,
            &[],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &Cancellation::new(),
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::DeadlineRequired
        ))
    ));
    assert!(matches!(
        provider.execute_with_previous(
            &request,
            None,
            &[],
            settings,
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &deadline(Duration::from_secs(30)),
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::MemoryEnforcementUnavailable
        ))
    ));
    assert_eq!(provider.stats().cache.entries, 0);

    let output = provider
        .execute_with_previous(
            &request,
            None,
            &[],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &deadline(Duration::from_secs(30)),
        )
        .expect("explicit M05 fallback admits incremental parsing");
    assert_eq!(
        output.output().memory_admission(),
        MemoryAdmissionStatus::UnavailableM05Fallback
    );
    assert!(output.previous().is_some());
}

#[test]
fn every_audited_grammar_parses_a_clean_representative_file() {
    let cases: [(&str, &str, &[u8]); 4] = [
        ("sample.rs", "rust", b"fn sample() {}\n"),
        ("sample.py", "python", b"def sample():\n    return None\n"),
        (
            "sample.js",
            "javascript",
            b"function sample() { return null; }\n",
        ),
        (
            "Sample.java",
            "java",
            b"class Sample { void sample() {} }\n",
        ),
    ];
    let limits = limits(MAX_SOURCE_BYTES, 1024, 64);
    let provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);

    for (name, language, source) in cases {
        let fixture = Fixture::new(name, source);
        let request = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            language,
            Vec::new(),
        );
        let output = execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &deadline(Duration::from_secs(30)),
        )
        .expect("audited representative syntax parses");
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete,
            "{language} should parse completely"
        );
        assert!(output.diagnostics().is_empty());
    }
}

#[test]
fn clean_malformed_and_invalid_utf8_are_error_tolerant_and_source_free() {
    let clean = Fixture::new("clean.rs", b"fn main() {}\n");
    let limits = limits(MAX_SOURCE_BYTES, 1024, 64);
    let provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    let clean_request = request(&clean.snapshot, &clean.source, &limits, "rust", Vec::new());

    let output = execute_parse(
        &provider,
        &clean_request,
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(Duration::from_secs(30)),
    )
    .expect("clean syntax commits");
    assert_eq!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(output.diagnostics().is_empty());

    let malformed = Fixture::new("malformed.rs", b"fn broken( {\n");
    let malformed_request = request(
        &malformed.snapshot,
        &malformed.source,
        &limits,
        "rust",
        Vec::new(),
    );
    let output = execute_parse(
        &provider,
        &malformed_request,
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(Duration::from_secs(30)),
    )
    .expect("malformed syntax preserves a partial result");
    assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
    assert_eq!(output.diagnostics().len(), 1);
    assert_eq!(
        output.diagnostics()[0].code().as_str(),
        "syntax-error-recovery"
    );

    let invalid = Fixture::new("invalid.rs", &[b'f', b'n', b' ', 0xff]);
    let invalid_request = request(
        &invalid.snapshot,
        &invalid.source,
        &limits,
        "rust",
        Vec::new(),
    );
    assert!(matches!(
        execute_parse(
            &provider,
            &invalid_request,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &deadline(Duration::from_secs(30))
        ),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "invalid-utf8"
    ));
}

#[test]
fn included_ranges_report_the_unparsed_file_gap() {
    let fixture = Fixture::new(
        "ranges.rs",
        b"fn first() {}\nthis is not rust\nfn second() {}\n",
    );
    let limits = limits(MAX_SOURCE_BYTES, 1024, 64);
    let provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    let end = u64::try_from(b"fn first() {}\n".len()).expect("fixture range fits");
    let included = IncludedRange::new(
        SourceSpan::new(fixture.snapshot.file(), 0, end).expect("range is ordered"),
        LanguageId::new("rust").expect("language is valid"),
    );
    let request = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "rust",
        vec![included],
    );

    let output = execute_parse(
        &provider,
        &request,
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(Duration::from_secs(30)),
    )
    .expect("included range parses");
    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
    assert_eq!(
        output.report().coverage().covered_source_bytes(),
        usize::try_from(end).expect("fixture range fits usize")
    );
    assert_eq!(output.report().coverage().skipped_regions(), 1);
}

#[test]
fn partial_traversal_does_not_count_absolute_included_range_offsets_as_coverage() {
    let mut source = vec![b' '; 100];
    source.extend_from_slice(b"fn ranged() {}\n".repeat(6).as_slice());
    source.resize(200, b' ');
    let fixture = Fixture::new("offset-range.rs", &source);
    let included = IncludedRange::new(
        SourceSpan::new(fixture.snapshot.file(), 100, 200).expect("range is ordered"),
        LanguageId::new("rust").expect("language is valid"),
    );
    let provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);

    for (max_nodes, max_depth, expected_code) in [
        (1, 64, "syntax-node-limit"),
        (1024, 1, "syntax-depth-limit"),
    ] {
        let limits = limits(MAX_SOURCE_BYTES, max_nodes, max_depth);
        let request = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            "rust",
            vec![included.clone()],
        );
        let output = execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &deadline(Duration::from_secs(30)),
        )
        .expect("bounded included range commits");

        assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
        assert_eq!(output.report().coverage().covered_source_bytes(), 0);
        assert_eq!(output.diagnostics()[0].code().as_str(), expected_code);
    }
}

#[test]
fn incremental_edit_reuses_only_an_exact_previous_identity() {
    let fixture = Fixture::new("incremental.rs", b"fn one() {}\n");
    let limits = limits(MAX_SOURCE_BYTES, 1024, 64);
    let provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    let settings = ParserSettings::new(64).expect("settings are bounded");
    let first_request = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "rust",
        Vec::new(),
    );
    let cancellation = deadline(Duration::from_secs(30));
    let first = provider
        .execute_with_previous(
            &first_request,
            None,
            &[],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        )
        .expect("initial parse succeeds");
    let previous = first
        .previous()
        .expect("initial tree fits the cache")
        .clone();

    let updated = fixture.rewrite(b"fn two() {}\n");
    let second_request = request(
        &updated.snapshot,
        &updated.source,
        &limits,
        "rust",
        Vec::new(),
    );
    let edit = SourceEdit::new(3, 6, "two").expect("edit is ordered");
    let second = provider
        .execute_with_previous(
            &second_request,
            Some(&previous),
            &[edit],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        )
        .expect("incremental parse succeeds");

    assert!(matches!(second.reuse_status(), ReuseStatus::Reused { .. }));
    assert_eq!(
        second.reuse_key().previous_content_hash(),
        Some(fixture.source.content_hash())
    );
    assert_eq!(
        second.reuse_key().current_content_hash(),
        updated.source.content_hash()
    );
    assert_eq!(second.reuse_key().edits().len(), 1);
    let edit_identity = second.reuse_key().edits()[0];
    assert_eq!(edit_identity.replacement_bytes(), 3);
    assert_eq!(edit_identity.replacement_hash(), content_hash(b"two"));
    assert!(!format!("{:?}", second.reuse_key()).contains("two"));
    assert_eq!(provider.stats().checked_out_parsers, 0);
    assert_eq!(provider.stats().available_parsers, 1);
}

#[test]
fn provider_and_parser_settings_mismatches_invalidate_reuse() {
    let fixture = Fixture::new("identity.rs", b"fn identity() {}\n");
    let limits = limits(MAX_SOURCE_BYTES, 1024, 64);
    let first_provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    let other_provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    let initial_settings = ParserSettings::new(64).expect("settings are bounded");
    let request = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "rust",
        Vec::new(),
    );
    let cancellation = deadline(Duration::from_secs(30));
    let initial = first_provider
        .execute_with_previous(
            &request,
            None,
            &[],
            initial_settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        )
        .expect("initial parse succeeds");
    let previous = initial.previous().expect("initial tree is cached").clone();

    let provider_mismatch = other_provider
        .execute_with_previous(
            &request,
            Some(&previous),
            &[],
            initial_settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        )
        .expect("provider mismatch falls back");
    assert_eq!(
        provider_mismatch.reuse_status(),
        ReuseStatus::Invalidated(ReuseInvalidation::Provider)
    );

    let settings_mismatch = first_provider
        .execute_with_previous(
            &request,
            Some(&previous),
            &[],
            ParserSettings::new(128).expect("alternate settings are bounded"),
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        )
        .expect("settings mismatch falls back");
    assert_eq!(
        settings_mismatch.reuse_status(),
        ReuseStatus::Invalidated(ReuseInvalidation::ParserSettings)
    );
}

#[test]
fn incremental_edit_work_is_bounded_before_cloning_or_intermediate_growth() {
    let fixture = Fixture::new("bounded.rs", b"abcdefgh");
    let limits = limits(8, 1024, 64);
    let provider = provider_with_edit_limit(8, 1024, 64, 4, 4096);
    let settings = ParserSettings::new(4).expect("settings fit the source bound");
    let request = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "rust",
        Vec::new(),
    );
    let cancellation = deadline(Duration::from_secs(30));
    let initial = provider
        .execute_with_previous(
            &request,
            None,
            &[],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        )
        .expect("initial bounded source parses");
    let previous = initial.previous().expect("initial tree is cached").clone();

    let insertion = SourceEdit::new(0, 0, "x").expect("test insertion is valid");
    assert!(matches!(
        provider.execute_with_previous(
            &request,
            None,
            std::slice::from_ref(&insertion),
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        ),
        Err(AdapterError::ProviderFailed { code })
            if code.as_str() == "incremental-edit-without-previous"
    ));

    let empty_edit = SourceEdit::new(0, 0, "").expect("empty replacement is valid");
    let too_many = vec![empty_edit; 5];
    assert!(matches!(
        provider.execute_with_previous(
            &request,
            Some(&previous),
            &too_many,
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        ),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "incremental-edit-limit"
    ));

    let oversized_replacement =
        SourceEdit::new(0, 0, "123456789").expect("replacement offsets are valid");
    assert!(matches!(
        provider.execute_with_previous(
            &request,
            Some(&previous),
            &[oversized_replacement],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        ),
        Err(AdapterError::ProviderFailed { code })
            if code.as_str() == "incremental-replacement-limit"
    ));

    let remove_insertion = SourceEdit::new(0, 1, "").expect("test deletion is valid");
    assert!(matches!(
        provider.execute_with_previous(
            &request,
            Some(&previous),
            &[insertion, remove_insertion],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        ),
        Err(AdapterError::ProviderFailed { code })
            if code.as_str() == "incremental-source-limit"
    ));
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn node_and_depth_limits_commit_explicit_partial_coverage() {
    let node_fixture = Fixture::new(
        "nodes.rs",
        b"fn main() { let one = 1; let two = 2; let three = 3; }\n",
    );
    let node_limits = limits(MAX_SOURCE_BYTES, 2, 64);
    let node_provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    let node_request = request(
        &node_fixture.snapshot,
        &node_fixture.source,
        &node_limits,
        "rust",
        Vec::new(),
    );
    let output = execute_parse(
        &node_provider,
        &node_request,
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(Duration::from_secs(30)),
    )
    .expect("node-limited parse commits");
    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
    assert_eq!(output.diagnostics()[0].code().as_str(), "syntax-node-limit");
    assert_eq!(output.report().resources().syntax_nodes(), 2);
    assert_eq!(node_provider.stats().cache.entries, 0);

    let deep_fixture = Fixture::new("deep.rs", b"fn main() { (((((((1))))))); }\n");
    let depth_limits = limits(MAX_SOURCE_BYTES, 1024, 2);
    let depth_provider = provider(MAX_SOURCE_BYTES, 1024, 64, 2 * 1024 * 1024);
    let depth_request = request(
        &deep_fixture.snapshot,
        &deep_fixture.source,
        &depth_limits,
        "rust",
        Vec::new(),
    );
    let output = execute_parse(
        &depth_provider,
        &depth_request,
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(Duration::from_secs(30)),
    )
    .expect("depth-limited parse commits");
    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
    assert_eq!(
        output.diagnostics()[0].code().as_str(),
        "syntax-depth-limit"
    );
    assert!(output.report().resources().max_syntax_depth() <= 2);
    assert_eq!(depth_provider.stats().cache.entries, 0);
}

#[test]
fn cache_byte_eviction_invalidates_an_old_handle() {
    let mut first_bytes = b"// ".to_vec();
    first_bytes.extend(std::iter::repeat_n(b'a', 8_000));
    let fixture = Fixture::new("cache.rs", &first_bytes);
    let limits = limits(16 * 1024, 1024, 64);
    let provider = provider(16 * 1024, 1024, 64, 10 * 1024);
    let settings = ParserSettings::new(1024).expect("settings are bounded");
    let first_request = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "rust",
        Vec::new(),
    );
    let cancellation = deadline(Duration::from_secs(30));
    let first = provider
        .execute_with_previous(
            &first_request,
            None,
            &[],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        )
        .expect("first cache parse succeeds");
    let old_handle = first.previous().expect("first entry fits").clone();

    let mut second_bytes = first_bytes.clone();
    second_bytes[3] = b'b';
    let updated = fixture.rewrite(&second_bytes);
    let second_request = request(
        &updated.snapshot,
        &updated.source,
        &limits,
        "rust",
        Vec::new(),
    );
    let edit = SourceEdit::new(3, 4, "b").expect("edit is valid");
    provider
        .execute_with_previous(
            &second_request,
            Some(&old_handle),
            &[edit],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        )
        .expect("second cache parse succeeds");
    assert_eq!(provider.stats().cache.entries, 1);

    let third = provider
        .execute_with_previous(
            &second_request,
            Some(&old_handle),
            &[],
            settings,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation,
        )
        .expect("evicted handle falls back");
    assert_eq!(
        third.reuse_status(),
        ReuseStatus::Invalidated(ReuseInvalidation::Evicted)
    );
}

#[test]
fn deadline_aborts_a_parser_bomb_and_releases_the_permit() {
    let mut bomb = b"fn bomb() {".to_vec();
    bomb.extend(std::iter::repeat_n(b'(', 3_500_000));
    bomb.extend(std::iter::repeat_n(b')', 3_500_000));
    bomb.extend_from_slice(b"}\n");
    let fixture = Fixture::new("bomb.rs", &bomb);
    let limits = limits(MAX_SOURCE_BYTES, 100_000, 1024);
    let provider = provider(MAX_SOURCE_BYTES, 100_000, 1024, 2 * 1024 * 1024);
    let request = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "rust",
        Vec::new(),
    );
    let cancellation = deadline(Duration::from_millis(50));

    assert!(matches!(
        execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &cancellation
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::DeadlineExceeded
        })
    ));
    let stats = provider.stats();
    assert_eq!(stats.checked_out_parsers, 0);
    assert_eq!(stats.pooled_parsers, 1);
    assert_eq!(stats.available_parsers, 1);
}

fn provider(
    max_source_bytes: usize,
    max_nodes: usize,
    max_depth: usize,
    cache_bytes: usize,
) -> TreeSitterProvider {
    provider_with_edit_limit(max_source_bytes, max_nodes, max_depth, 64, cache_bytes)
}

fn provider_with_edit_limit(
    max_source_bytes: usize,
    max_nodes: usize,
    max_depth: usize,
    max_incremental_edits: usize,
    cache_bytes: usize,
) -> TreeSitterProvider {
    let settings =
        ParserSettings::new(max_source_bytes.min(1024)).expect("test settings are valid");
    let config = RuntimeConfig::new(
        max_source_bytes,
        max_nodes,
        max_depth,
        16,
        max_incremental_edits,
        1,
        cache_bytes,
        settings,
    )
    .expect("test runtime config is valid");
    TreeSitterProvider::new(config).expect("audited provider initializes")
}

fn limits(max_source_bytes: usize, max_nodes: usize, max_depth: usize) -> AnalysisLimits {
    let batch = BatchThresholds::new(128, 256 * 1024, 8, 4096).expect("batch limits are valid");
    let stream = StreamLimits::new(64, 4096, 4 * 1024 * 1024, 64, 64 * 1024, 128 * 1024, batch)
        .expect("stream limits are valid");
    AnalysisLimits::new(
        max_source_bytes,
        max_nodes,
        max_depth,
        16,
        8 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .expect("analysis limits are valid")
}

fn request<'a>(
    snapshot: &'a SourceSnapshot,
    source: &SourceRef,
    limits: &'a AnalysisLimits,
    language: &str,
    included_ranges: Vec<IncludedRange>,
) -> ParseRequest<'a> {
    ParseRequest::new(
        GenerationBoundSnapshot::new(snapshot, source).expect("snapshot binds"),
        LanguageId::new(language).expect("test language is valid"),
        EncodingId::new("utf-8").expect("test encoding is valid"),
        included_ranges,
        limits,
    )
    .expect("parse request is valid")
}

fn deadline(duration: Duration) -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(duration)
            .expect("test deadline is representable"),
    )
}

struct Fixture {
    temporary: Arc<TempDir>,
    relative: RelativePath,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(name: &str, bytes: &[u8]) -> Self {
        let current = std::env::current_dir().expect("current directory exists");
        let temporary =
            Arc::new(tempdir_in(current).expect("local temporary directory is available"));
        fs::write(temporary.path().join(name), bytes).expect("fixture source is written");
        let relative = RelativePath::parse(Path::new(name)).expect("fixture path is valid");
        let (snapshot, source) = capture(&temporary, &relative);
        Self {
            temporary,
            relative,
            snapshot,
            source,
        }
    }

    fn rewrite(&self, bytes: &[u8]) -> Self {
        fs::write(self.temporary.path().join(self.relative.as_str()), bytes)
            .expect("updated fixture source is written");
        let (snapshot, source) = capture(&self.temporary, &self.relative);
        Self {
            temporary: Arc::clone(&self.temporary),
            relative: self.relative.clone(),
            snapshot,
            source,
        }
    }
}

fn capture(temporary: &TempDir, relative: &RelativePath) -> (SourceSnapshot, SourceRef) {
    let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
        .parse()
        .expect("repository identity parses");
    let repository =
        RepositoryRoot::open(repository_id, temporary.path()).expect("temporary root opens");
    let snapshot = repository
        .snapshot(
            relative,
            u64::try_from(MAX_SOURCE_BYTES).expect("limit fits"),
        )
        .expect("fixture snapshot is stable");
    let end = u64::try_from(snapshot.content().len()).expect("fixture length fits");
    let source = SourceRef::new(
        repository_id,
        "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
            .parse()
            .expect("generation identity parses"),
        SourceSpan::new(snapshot.file(), 0, end).expect("full span is ordered"),
        snapshot.content_hash(),
        None,
    );
    (snapshot, source)
}
