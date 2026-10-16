//! Literal evidence remains generation-pinned across paging and durable reopen.
//! Exact source reads provide an independent check of returned physical spans.

use std::{fs, time::Instant};

use rootlight_cancel::CancellationReason;
use rootlight_ids::content_hash;
use rootlight_ir::SkippedRegionReason;
use rootlight_runtime::RuntimePaths;
use rootlight_source::{SourceEncoding, SourceFindStop, SourceReadOptions};
use rootlight_vfs::TextEncoding;

use super::*;
use crate::{FirstSliceBudget, FirstSliceCoverageGapReason};

fn temporary() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap()
}

fn file_id(service: &FirstSliceService, generation: GenerationId, path: &str) -> FileId {
    service
        .loaded_generation_snapshot(generation)
        .unwrap()
        .document()
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap()
        .id
}

#[test]
fn coverage_aliases_preserve_the_same_source_scoped_gaps() {
    let fixture = temporary();
    fs::write(
        fixture.path().join("proof.v"),
        b"Definition written_value : nat := 1.\n",
    )
    .unwrap();
    fs::write(fixture.path().join("unrecognized.note"), b"plain source\n").unwrap();
    let broken_cpp = b"int broken( {\n";
    fs::write(fixture.path().join("broken.cpp"), broken_cpp).unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let cpp_file = snapshot
        .document()
        .files
        .iter()
        .find(|file| file.path == "broken.cpp")
        .unwrap();
    assert_eq!(cpp_file.language, "cpp");
    assert_eq!(cpp_file.content_hash, content_hash(broken_cpp));
    assert!(snapshot.document().skipped_regions.iter().any(|gap| {
        gap.reason == SkippedRegionReason::ParseError
            && gap.source.span().file() == cpp_file.id
            && gap.source.content_hash() == cpp_file.content_hash
    }));
    let summary = |languages: &[String]| {
        service
            .coverage_gap_summary_for_paths_until(
                receipt.repository,
                receipt.generation,
                languages,
                &[],
                &cancellation,
            )
            .unwrap()
    };
    for capability in rootlight_discovery::language_capabilities() {
        let canonical = summary(&[capability.language.to_owned()]);
        for alias in capability.aliases {
            assert_eq!(summary(&[(*alias).to_owned()]), canonical, "alias {alias}");
        }
    }
    let coq = summary(&["coq".to_owned()]);
    assert!(coq.gaps.iter().any(|gap| {
        gap.reason == FirstSliceCoverageGapReason::Unsupported
            && gap.language.as_deref() == Some("coq")
            && gap.files == 1
    }));
    assert_eq!(summary(&["coq".to_owned(), "rocq".to_owned()]), coq);
    let cpp = summary(&["cpp".to_owned()]);
    assert!(cpp.gaps.iter().any(|gap| {
        gap.reason == FirstSliceCoverageGapReason::ParseError
            && gap.language.as_deref() == Some("cpp")
            && gap.files == 1
    }));
    assert_eq!(summary(&["cplusplus".to_owned()]), cpp);
    let unknown = summary(&["unknown".to_owned()]);
    assert!(unknown.gaps.iter().any(|gap| {
        gap.reason == FirstSliceCoverageGapReason::Unrecognized
            && gap.language.as_deref() == Some("unknown")
            && gap.files == 1
    }));
    assert!(
        !summary(&["custom.language-v2".to_owned()])
            .gaps
            .iter()
            .any(|gap| gap.language.is_some())
    );
}

#[test]
fn snapshot_preparation_spends_the_same_source_deadline_as_matching() {
    let maximum = Duration::from_secs(2);
    assert_eq!(
        remaining_literal_duration(maximum, Duration::from_millis(1750)).unwrap(),
        Duration::from_millis(250),
    );
    for elapsed in [maximum, Duration::from_secs(3), Duration::MAX] {
        assert!(matches!(
            remaining_literal_duration(maximum, elapsed),
            Err(SourceFindError::Source(SourceError::Cancelled(
                CancellationReason::DeadlineExceeded
            )))
        ));
    }
}

#[test]
fn literal_pages_and_exact_reads_keep_published_bytes_after_live_changes() {
    let fixture = temporary();
    let original = b"// aaaaa\npub fn entry() {}\n";
    fs::write(fixture.path().join("sample.rs"), original).unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    let file = file_id(&service, receipt.generation, "sample.rs");
    fs::write(fixture.path().join("sample.rs"), b"changed live bytes").unwrap();
    let mut budget = SourceFindBudget::new();
    budget.matcher.max_scan_bytes = 1;
    budget.matcher.max_matches = 1;
    let mut cursor = None;
    let mut position = 0;
    let mut matches = Vec::new();
    loop {
        let result = service
            .find_literal(
                receipt.generation,
                file,
                "aaa",
                cursor,
                budget,
                &cancellation,
            )
            .unwrap();
        assert_eq!(result.generation, receipt.generation);
        assert_eq!(result.file.id, file);
        assert_eq!(result.scanned.start, position);
        assert_eq!(result.scanned.end, position + 1);
        position = result.scanned.end;
        matches.extend(result.matches);
        cursor = result.continuation;
        if cursor.is_none() {
            assert_eq!(result.stop, SourceFindStop::EndOfSource);
            break;
        }
    }
    assert_eq!(position, original.len() as u64);
    assert_eq!(
        matches
            .iter()
            .map(|reference| (reference.span().start_byte(), reference.span().end_byte()))
            .collect::<Vec<_>>(),
        [(3, 6), (4, 7), (5, 8)]
    );
    for reference in matches {
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![reference.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].reference, reference);
        assert_eq!(read.data.chunks[0].bytes, b"aaa");
    }
    let zero = service
        .find_literal(
            receipt.generation,
            file,
            "absent",
            None,
            SourceFindBudget::new(),
            &cancellation,
        )
        .unwrap();
    assert!(zero.matches.is_empty());
    assert_eq!(zero.stop, SourceFindStop::EndOfSource);
}

#[test]
fn literal_durable_reopen_retains_three_encodings_and_original_source_identity() {
    let storage = temporary();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = temporary();
    let original = "// λ😀λ😀λ\npub fn example() {}\n";
    let query = "λ😀λ";
    let mut inputs = Vec::new();
    for (path, encoding) in [
        ("utf8.rs", TextEncoding::Utf8),
        ("little.rs", TextEncoding::Utf16Le),
        ("big.rs", TextEncoding::Utf16Be),
    ] {
        let bytes: Vec<_> = match encoding {
            TextEncoding::Utf8 => original.as_bytes().to_vec(),
            TextEncoding::Utf16Le => std::iter::once(0xfeff)
                .chain(original.encode_utf16())
                .flat_map(u16::to_le_bytes)
                .collect(),
            TextEncoding::Utf16Be => std::iter::once(0xfeff)
                .chain(original.encode_utf16())
                .flat_map(u16::to_be_bytes)
                .collect(),
        };
        fs::write(fixture.path().join(path), &bytes).unwrap();
        inputs.push((path, encoding, bytes));
    }
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    let expected: Vec<_> = inputs
        .iter()
        .map(|(path, _, _)| {
            let file = file_id(&service, receipt.generation, path);
            service
                .find_literal(
                    receipt.generation,
                    file,
                    query,
                    None,
                    SourceFindBudget::new(),
                    &cancellation,
                )
                .unwrap()
        })
        .collect();
    drop(service);
    for (path, _, _) in &inputs {
        fs::write(
            fixture.path().join(path),
            b"live bytes changed after publication",
        )
        .unwrap();
    }
    let service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    for ((_, encoding, bytes), expected) in inputs.iter().zip(expected) {
        let actual = service
            .find_literal(
                receipt.generation,
                expected.file.id,
                query,
                None,
                SourceFindBudget::new(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.file.encoding, *encoding);
        assert_eq!(actual.file.content_hash, rootlight_ids::content_hash(bytes));
        assert_eq!(actual.matches.len(), 2);
        let expected_match: Vec<_> = match encoding {
            TextEncoding::Utf8 => query.as_bytes().to_vec(),
            TextEncoding::Utf16Le => query.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            TextEncoding::Utf16Be => query.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        };
        for reference in actual.matches {
            assert_eq!(
                &bytes[usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap()],
                expected_match
            );
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![reference],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(SourceEncoding::Bytes),
                    FirstSliceBudget::default(),
                    &cancellation,
                )
                .unwrap();
            assert_eq!(read.data.chunks[0].bytes, expected_match);
        }
    }
}

#[test]
fn literal_direct_errors_preserve_query_cursor_file_budget_and_cancellation_categories() {
    let fixture = temporary();
    fs::write(
        fixture.path().join("sample.rs"),
        b"// xxxxxa\npub fn example() {}\n",
    )
    .unwrap();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    let file = file_id(&service, receipt.generation, "sample.rs");
    let unknown_generation = GenerationId::from_bytes([237; 20]);
    assert!(matches!(
        service.find_literal(
            unknown_generation,
            file,
            "",
            None,
            SourceFindBudget::new(),
            &cancellation
        ),
        Err(FirstSliceSourceFindError::Find(SourceFindError::Literal(
            LiteralSearchError::EmptyQuery
        )))
    ));
    let mut budget = SourceFindBudget::new();
    budget.matcher.max_query_bytes = 2;
    assert!(matches!(
        service.find_literal(
            unknown_generation,
            file,
            "abcd",
            None,
            budget,
            &cancellation
        ),
        Err(FirstSliceSourceFindError::Find(SourceFindError::Literal(
            LiteralSearchError::QueryLimit
        )))
    ));
    assert!(matches!(
        service.find_literal(
            receipt.generation,
            FileId::from_bytes([238; 20]),
            "a",
            None,
            SourceFindBudget::new(),
            &cancellation
        ),
        Err(FirstSliceSourceFindError::Find(SourceFindError::Source(
            SourceError::FileNotFound
        )))
    ));
    assert!(matches!(
        service.find_literal(
            receipt.generation,
            file,
            "abcda",
            Some(SourceFindCursor {
                next_byte: 8,
                matched_prefix: 4
            }),
            SourceFindBudget::new(),
            &cancellation
        ),
        Err(FirstSliceSourceFindError::Find(SourceFindError::Literal(
            LiteralSearchError::InvalidCursor
        )))
    ));
    budget = SourceFindBudget::new();
    budget.source.max_snapshot_bytes = 1;
    assert!(matches!(
        service.find_literal(receipt.generation, file, "a", None, budget, &cancellation),
        Err(FirstSliceSourceFindError::Find(SourceFindError::Source(
            SourceError::SnapshotBudgetExceeded
        )))
    ));
    let cancelled = Cancellation::with_deadline(Instant::now());
    assert!(matches!(
        service.find_literal(
            receipt.generation,
            file,
            "a",
            None,
            SourceFindBudget::new(),
            &cancelled
        ),
        Err(FirstSliceSourceFindError::Service(
            FirstSliceError::Cancelled(CancellationReason::DeadlineExceeded)
        ))
    ));
}
