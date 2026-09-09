//! Native inline work admission and original-coordinate regression tests.
//! Real VFS snapshots exercise the pooled parser, including abort and reuse.

use super::*;
use rootlight_adapter_sdk::{
    AnalysisLimits, BatchThresholds, GenerationBoundSnapshot, StreamLimits,
};
use rootlight_cancel::CancellationReason;
use rootlight_ir::IrLimits;
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};

struct Fixture {
    _directory: tempfile::TempDir,
    snapshot: SourceSnapshot,
    source: SourceRef,
    limits: AnalysisLimits,
    provider: TreeSitterProvider,
}

impl Fixture {
    fn new(text: &str) -> Self {
        let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        std::fs::write(directory.path().join("document.md"), text).unwrap();
        let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
        let repository = RepositoryRoot::open(repository_id, directory.path()).unwrap();
        let relative = RelativePath::parse(std::path::Path::new("document.md")).unwrap();
        let snapshot = repository.snapshot(&relative, 1024 * 1024).unwrap();
        let source = SourceRef::new(
            repository_id,
            "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
                .parse()
                .unwrap(),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let batch = BatchThresholds::new(128, 256 * 1024, 8, 4096).unwrap();
        let stream =
            StreamLimits::new(64, 4096, 4 * 1024 * 1024, 64, 64 * 1024, 128 * 1024, batch).unwrap();
        let limits = AnalysisLimits::new(
            1024 * 1024,
            100_000,
            64,
            16,
            8 * 1024 * 1024,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .unwrap();
        let config = RuntimeConfig::new(
            1024 * 1024,
            100_000,
            64,
            16,
            16,
            1,
            2 * 1024 * 1024,
            ParserSettings::new(1024).unwrap(),
        )
        .unwrap();
        Self {
            _directory: directory,
            snapshot,
            source,
            limits,
            provider: TreeSitterProvider::new(config).unwrap(),
        }
    }

    fn request(&self, spans: &[(u64, u64)]) -> ParseRequest<'_> {
        let ranges = spans
            .iter()
            .map(|&(start, end)| {
                IncludedRange::new(
                    SourceSpan::new(self.snapshot.file(), start, end).unwrap(),
                    LanguageId::new("markdown").unwrap(),
                )
            })
            .collect();
        ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            LanguageId::new("markdown").unwrap(),
            EncodingId::new("utf-8").unwrap(),
            ranges,
            &self.limits,
        )
        .unwrap()
    }

    fn parse(
        &self,
        context: &mut NativeParseContext,
        cancellation: &Cancellation,
    ) -> Result<Tree, AdapterError> {
        self.provider.parse_native_tree(
            &self.request(&[]),
            &tree_sitter_md::INLINE_LANGUAGE.into(),
            None,
            self.provider.config.default_settings(),
            Some(context),
            cancellation,
        )
    }
}

fn context(checks: usize) -> NativeParseContext {
    NativeParseContext {
        range_origin: (0, Point { row: 0, column: 0 }),
        remaining_progress_checks: checks,
    }
}

#[test]
fn shared_native_work_survives_calls_and_returns_the_parser_after_exhaustion() {
    let fixture = Fixture::new(&"[link](target.md) ".repeat(1024));
    let cancellation = Cancellation::new();
    let mut calibration = context(usize::MAX);
    let expected = fixture.parse(&mut calibration, &cancellation).unwrap();
    assert!(!expected.root_node().has_error());
    let consumed = usize::MAX - calibration.remaining_progress_checks;
    assert!(consumed > 1);

    let mut shared = context(consumed + 1);
    assert!(fixture.parse(&mut shared, &cancellation).is_ok());
    assert_eq!(shared.remaining_progress_checks, 1);
    assert!(
        matches!(fixture.parse(&mut shared, &cancellation), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "syntax-depth-parse-work-limit")
    );
    assert_eq!(shared.remaining_progress_checks, 0);
    assert_eq!(fixture.provider.pool.stats().checked_out, 0);
    assert_eq!(fixture.provider.pool.stats().created, 1);

    let recovered = fixture
        .parse(&mut context(consumed + 1), &cancellation)
        .unwrap();
    assert_eq!(
        recovered.root_node().to_sexp(),
        expected.root_node().to_sexp()
    );
    assert_eq!(
        recovered.root_node().byte_range(),
        expected.root_node().byte_range()
    );
    assert_eq!(fixture.provider.pool.stats().created, 1);
    assert_eq!(fixture.provider.pool.stats().checked_out, 0);
}

#[test]
fn exhausted_native_work_rejects_tiny_inputs_before_callbacks() {
    let fixture = Fixture::new("short");
    assert!(
        matches!(fixture.parse(&mut context(0), &Cancellation::new()), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "syntax-depth-parse-work-limit")
    );
    assert_eq!(fixture.provider.pool.stats().created, 0);
}

#[test]
fn cancellation_precedes_shared_exhaustion_and_does_not_poison_reuse() {
    let fixture = Fixture::new("[link](target.md)");
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        fixture.parse(&mut context(0), &cancellation),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert_eq!(fixture.provider.pool.stats().checked_out, 0);
    assert!(
        fixture
            .parse(&mut context(1024), &Cancellation::new())
            .is_ok()
    );
    assert_eq!(fixture.provider.pool.stats().checked_out, 0);
}

#[test]
fn native_origin_matches_full_scan_across_unicode_and_crlf_ranges() {
    let text = "🚀\r\n> é [first](one.md)\r\n> [second](two.md)";
    let fixture = Fixture::new(text);
    let first = text.find('é').unwrap();
    let split = text.find("\r\n> [second]").unwrap();
    let second = text.find("[second]").unwrap();
    let request = fixture.request(&[
        (u64::try_from(first).unwrap(), u64::try_from(split).unwrap()),
        (
            u64::try_from(second).unwrap(),
            u64::try_from(text.len()).unwrap(),
        ),
    ]);
    let cancellation = Cancellation::new();
    let baseline = tree_sitter_ranges(&request, text.as_bytes(), None, &cancellation).unwrap();
    let anchored = tree_sitter_ranges(
        &request,
        text.as_bytes(),
        Some((first, Point { row: 1, column: 2 })),
        &cancellation,
    )
    .unwrap();
    assert_eq!(anchored, baseline);
    assert_eq!(anchored[1].start_point, Point { row: 2, column: 2 });
    assert!(
        tree_sitter_ranges(
            &request,
            text.as_bytes(),
            Some((first + 1, Point { row: 1, column: 3 })),
            &cancellation
        )
        .is_err()
    );
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        tree_sitter_ranges(&request, text.as_bytes(), None, &cancellation),
        Err(AdapterError::Cancelled { .. })
    ));
}
