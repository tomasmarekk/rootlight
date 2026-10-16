//! Source-service literal contracts reuse the existing generation/VFS fixtures.
//! Match evidence is read through the ordinary exact-byte API as a separate oracle.

use super::*;
use rootlight_vfs::TextEncoding;

fn find(
    fixture: &Fixture,
    query: &str,
    cursor: Option<SourceFindCursor>,
    budget: SourceFindBudget,
) -> Result<SourceFindResult, SourceFindError> {
    SourceService::new(&fixture.root, &fixture.generation)?.find_literal(
        fixture.reference.span().file(),
        query,
        cursor,
        budget,
        &Cancellation::new(),
    )
}

#[test]
fn default_find_limits_preserve_independent_source_and_matcher_admission() {
    let budget = SourceFindBudget::new();
    assert_eq!(budget.source, SourceBudget::new());
    assert_eq!(budget.source.max_snapshot_bytes, 64 * 1024 * 1024);
    assert_eq!(budget.source.max_duration, Duration::from_secs(2));
    assert_eq!(budget.matcher.max_query_bytes, 64 * 1024);
    assert_eq!(budget.matcher.max_scan_bytes, 64 * 1024 * 1024);
    assert_eq!(budget.matcher.max_matches, 128);
    assert_eq!(budget.matcher.max_memory_bytes, 2 * 1024 * 1024);
    assert_eq!(budget.matcher.max_restore_bytes, 256 * 1024);
}

#[test]
fn literal_matches_round_trip_exact_source_refs_in_all_physical_encodings() {
    let original = "\u{feff}prefix λ😀λ😀λ suffix\r\n";
    let query = "λ😀λ";
    for encoding in [
        TextEncoding::Utf8,
        TextEncoding::Utf16Le,
        TextEncoding::Utf16Be,
    ] {
        let bytes = match encoding {
            TextEncoding::Utf8 => original.as_bytes().to_vec(),
            _ => physical_utf16(original, encoding),
        };
        let fixture = super::fixture(&bytes, encoding.as_str(), (0, bytes.len() as u64));
        let result = find(&fixture, query, None, SourceFindBudget::new()).unwrap();
        assert_eq!(result.matches.len(), 2);
        assert_eq!(
            result.scanned,
            if encoding == TextEncoding::Utf8 { 0 } else { 2 }..bytes.len() as u64
        );
        assert_eq!(result.stop, SourceFindStop::EndOfSource);
        assert!(result.continuation.is_none());
        assert_eq!(result.restoration_bytes, 0);
        assert_eq!(result.file.id, fixture.reference.span().file());
        assert_eq!(result.file.content_hash, content_hash(&bytes));
        assert_eq!(result.file.encoding, encoding);
        assert_eq!(result.file.path, "src/sample.rs");
        assert_eq!(result.file.language, "rust");
        assert_eq!(result.file.trust, SourceTrust::UntrustedRepositoryData);
        assert!(!format!("{result:?}").contains("src/sample.rs"));
        let expected_bytes: Vec<_> = match encoding {
            TextEncoding::Utf8 => query.as_bytes().to_vec(),
            TextEncoding::Utf16Le => query.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            TextEncoding::Utf16Be => query.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        };
        for reference in &result.matches {
            assert_eq!(reference.repository(), fixture.reference.repository());
            assert_eq!(reference.generation(), fixture.reference.generation());
            assert_eq!(reference.content_hash(), fixture.reference.content_hash());
            assert!(reference.line_hint().is_none());
            let raw = read(
                &fixture,
                std::slice::from_ref(reference),
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(raw.chunks[0].bytes, expected_bytes);
        }
        let path = RelativePath::parse(Path::new("src/sample.rs")).unwrap();
        let snapshots = [Arc::new(
            fixture.root.snapshot(&path, bytes.len() as u64).unwrap(),
        )];
        let retained = SourceService::from_snapshots(&snapshots, &fixture.generation)
            .unwrap()
            .find_literal(
                fixture.reference.span().file(),
                query,
                None,
                SourceFindBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(retained, result);
    }
}

#[test]
fn literal_paging_conserves_overlaps_forward_coverage_and_single_restore_cost() {
    let fixture = super::fixture(b"abababa", "utf-8", (0, 7));
    let mut budget = SourceFindBudget::new();
    budget.matcher.max_scan_bytes = 1;
    let mut cursor = None;
    let mut position = 0;
    let mut spans = Vec::new();
    loop {
        let page = find(&fixture, "aba", cursor, budget).unwrap();
        assert_eq!(page.scanned, position..position + 1);
        assert_eq!(
            page.restoration_bytes,
            usize::try_from(position.min(3)).unwrap()
        );
        position += 1;
        spans.extend(page.matches.into_iter().map(|reference| reference.span()));
        cursor = page.continuation;
        if cursor.is_none() {
            assert_eq!(page.stop, SourceFindStop::EndOfSource);
            assert_eq!(position, 7);
            break;
        }
        assert_eq!(page.stop, SourceFindStop::ScanLimit);
        assert_eq!(
            cursor.unwrap().next_byte,
            usize::try_from(position).unwrap()
        );
    }
    assert_eq!(
        spans,
        [
            SourceSpan::new(fixture.reference.span().file(), 0, 3).unwrap(),
            SourceSpan::new(fixture.reference.span().file(), 2, 5).unwrap(),
            SourceSpan::new(fixture.reference.span().file(), 4, 7).unwrap()
        ]
    );
    let none = find(&fixture, "absent", None, SourceFindBudget::new()).unwrap();
    assert!(none.matches.is_empty());
    assert_eq!(none.stop, SourceFindStop::EndOfSource);
    assert_eq!(none.scanned, 0..7);
    let empty = super::fixture(b"", "utf-8", (0, 0));
    let result = find(&empty, "a", None, SourceFindBudget::new()).unwrap();
    assert!(result.matches.is_empty());
    assert_eq!(result.scanned, 0..0);
    assert_eq!(result.stop, SourceFindStop::EndOfSource);
}

#[test]
fn literal_response_limits_do_not_cap_source_admission_or_mislabel_continuations() {
    let mut bytes = vec![b'x'; 256 * 1024];
    bytes.extend_from_slice(b"tail_literal");
    let fixture = super::fixture(&bytes, "utf-8", (0, bytes.len() as u64));
    let mut budget = SourceFindBudget::new();
    budget.source.max_source_bytes = 1;
    budget.source.max_response_memory_bytes = 1024;
    let page = find(&fixture, "tail_literal", None, budget).unwrap();
    assert_eq!(page.matches[0].span().start_byte(), 256 * 1024);
    assert_eq!(page.matches[0].span().end_byte(), bytes.len() as u64);
    assert!(page.total_response_memory_bytes <= 1024);
    budget.source.max_snapshot_bytes = bytes.len() as u64 - 1;
    assert!(matches!(
        find(&fixture, "tail_literal", None, budget),
        Err(SourceFindError::Source(SourceError::SnapshotBudgetExceeded))
    ));

    let fixture = super::fixture(b"aaaa", "utf-8", (0, 4));
    let mut budget = SourceFindBudget::new();
    let metadata = "src/sample.rs".len() + "rust".len();
    budget.source.max_response_memory_bytes = metadata + std::mem::size_of::<SourceRef>();
    let page = find(&fixture, "a", None, budget).unwrap();
    assert_eq!(page.matches.len(), 1);
    assert_eq!(page.stop, SourceFindStop::ResponseMemoryLimit);
    assert_eq!(page.continuation.unwrap().next_byte, 1);
    assert_eq!(
        page.total_response_memory_bytes,
        budget.source.max_response_memory_bytes
    );
    budget.matcher.max_matches = 1;
    assert_eq!(
        find(&fixture, "a", None, budget).unwrap().stop,
        SourceFindStop::MatchLimit
    );
    budget = SourceFindBudget::new();
    budget.matcher.max_memory_bytes = std::mem::size_of::<char>()
        + std::mem::size_of::<usize>()
        + std::mem::size_of::<std::ops::Range<usize>>();
    assert_eq!(
        find(&fixture, "a", None, budget).unwrap().stop,
        SourceFindStop::MatcherMemoryLimit
    );
}

#[test]
fn literal_live_and_retained_identity_checks_reject_stale_unknown_and_foreign_files() {
    let fixture = super::fixture(b"first first", "utf-8", (0, 11));
    let path = RelativePath::parse(Path::new("src/sample.rs")).unwrap();
    let snapshots = [Arc::new(fixture.root.snapshot(&path, 11).unwrap())];
    let original = find(&fixture, "first", None, SourceFindBudget::new()).unwrap();
    fs::write(
        fixture._temporary.path().join("src/sample.rs"),
        b"other other",
    )
    .unwrap();
    assert!(matches!(
        find(&fixture, "first", None, SourceFindBudget::new()),
        Err(SourceFindError::Source(SourceError::StaleSource))
    ));
    let retained = SourceService::from_snapshots(&snapshots, &fixture.generation).unwrap();
    assert_eq!(
        retained
            .find_literal(
                fixture.reference.span().file(),
                "first",
                None,
                SourceFindBudget::new(),
                &Cancellation::new()
            )
            .unwrap(),
        original
    );
    let stale_snapshots = [Arc::new(fixture.root.snapshot(&path, 11).unwrap())];
    assert!(matches!(
        SourceService::from_snapshots(&stale_snapshots, &fixture.generation)
            .unwrap()
            .find_literal(
                fixture.reference.span().file(),
                "first",
                None,
                SourceFindBudget::new(),
                &Cancellation::new()
            ),
        Err(SourceFindError::Source(SourceError::StaleSource))
    ));
    let unknown = fixture
        .root
        .file_id(&RelativePath::parse(Path::new("unknown.rs")).unwrap());
    let foreign_root = RepositoryRoot::open(
        derive_repository(b"foreign-find-repository").id(),
        fixture._temporary.path(),
    )
    .unwrap();
    let foreign = foreign_root.file_id(&path);
    for file in [unknown, foreign] {
        assert!(matches!(
            retained.find_literal(
                file,
                "first",
                None,
                SourceFindBudget::new(),
                &Cancellation::new()
            ),
            Err(SourceFindError::Source(SourceError::FileNotFound))
        ));
    }
    assert!(matches!(
        SourceService::new(&foreign_root, &fixture.generation),
        Err(SourceError::RepositoryMismatch)
    ));
    assert!(matches!(
        SourceService::from_snapshots(&[], &fixture.generation)
            .unwrap()
            .find_literal(
                fixture.reference.span().file(),
                "first",
                None,
                SourceFindBudget::new(),
                &Cancellation::new()
            ),
        Err(SourceFindError::Source(SourceError::FileNotFound))
    ));
}

#[test]
fn literal_query_and_control_failures_precede_vfs_and_preserve_error_categories() {
    let fixture = super::fixture(b"source", "utf-8", (0, 6));
    fs::remove_file(fixture._temporary.path().join("src/sample.rs")).unwrap();
    let mut budget = SourceFindBudget::new();
    budget.matcher.max_query_bytes = 3;
    assert!(matches!(
        find(&fixture, "source", None, budget),
        Err(SourceFindError::Literal(LiteralSearchError::QueryLimit))
    ));
    assert!(matches!(
        find(&fixture, "", None, budget),
        Err(SourceFindError::Literal(LiteralSearchError::EmptyQuery))
    ));
    budget = SourceFindBudget::new();
    budget.matcher.max_memory_bytes = 1;
    assert!(matches!(
        find(&fixture, "source", None, budget),
        Err(SourceFindError::Literal(LiteralSearchError::MemoryLimit))
    ));
    let cancelled = Cancellation::with_deadline(Instant::now());
    assert!(matches!(
        SourceService::new(&fixture.root, &fixture.generation)
            .unwrap()
            .find_literal(
                fixture.reference.span().file(),
                "source",
                None,
                SourceFindBudget::new(),
                &cancelled
            ),
        Err(SourceFindError::Source(SourceError::Cancelled(
            CancellationReason::DeadlineExceeded
        )))
    ));
}

#[test]
fn literal_cursor_and_encoding_checks_never_return_prefix_only_success() {
    let fixture = super::fixture(b"xxxxxa", "utf-8", (0, 6));
    assert!(matches!(
        find(
            &fixture,
            "abcda",
            Some(SourceFindCursor {
                next_byte: 5,
                matched_prefix: 4
            }),
            SourceFindBudget::new()
        ),
        Err(SourceFindError::Literal(LiteralSearchError::InvalidCursor))
    ));
    for (bytes, encoding) in [
        (vec![0xff, 0xfe, b'a', 0, 0, 0xd8], "utf-16le"),
        (b"valid".to_vec(), "utf-16le"),
        (vec![0xff, 0xfe, b'a', 0], "utf-8"),
    ] {
        let fixture = super::fixture(&bytes, encoding, (0, bytes.len() as u64));
        assert!(matches!(
            find(&fixture, "a", None, SourceFindBudget::new()),
            Err(SourceFindError::Source(SourceError::EncodingUnsupported))
        ));
    }
    let bytes = physical_utf16("😀x", TextEncoding::Utf16Le);
    let fixture = super::fixture(&bytes, "utf-16le", (0, bytes.len() as u64));
    assert!(matches!(
        find(
            &fixture,
            "x",
            Some(SourceFindCursor {
                next_byte: 4,
                matched_prefix: 0
            }),
            SourceFindBudget::new()
        ),
        Err(SourceFindError::Literal(LiteralSearchError::InvalidCursor))
    ));
}

#[test]
fn literal_metadata_preserves_extraction_origin_without_rebasing_match_spans() {
    let mut fixture = super::fixture(b"derived literal source", "utf-8", (0, 22));
    let container = add_fixture_file(
        &mut fixture,
        "source.container",
        b"opaque container bytes",
        (0, 22),
    );
    let metadata = fixture.generation.metadata();
    let mut document = fixture.generation.clone().into_document();
    document.version = rootlight_ir::NormalizedIrVersion::V1_6;
    document
        .files
        .iter_mut()
        .find(|file| file.id == fixture.reference.span().file())
        .unwrap()
        .generated = true;
    document
        .source_mappings
        .push(rootlight_ir::SourceMappingRecord {
            id: derive_fact("rootlight.source-mapping/v1", b"literal-test-extraction").id(),
            repository: metadata.repository(),
            generation: metadata.generation(),
            from: fixture.reference.clone(),
            to: container.clone(),
            kind: SourceMappingKind::ExtractedFromContainer,
            provenance: document.provenance[0].id,
            evidence: FactEvidence {
                source: Some(container.clone()),
                derivation: Vec::new(),
            },
        });
    fixture.generation = GenerationSnapshot::new(
        metadata,
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let result = find(&fixture, "literal", None, SourceFindBudget::new()).unwrap();
    assert_eq!(result.file.extracted_from, Some(container));
    assert!(result.file.generated);
    assert_eq!(result.matches.len(), 1);
    assert_eq!(
        result.matches[0].span(),
        SourceSpan::new(fixture.reference.span().file(), 8, 15).unwrap()
    );
    assert_eq!(
        result.matches[0].content_hash(),
        fixture.reference.content_hash()
    );
}
