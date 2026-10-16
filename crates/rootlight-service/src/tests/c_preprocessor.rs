//! A native C projection refusal preserves publication and exact source access.
//! The unsupported file remains explicit beside an independently analyzed file,
//! including durable reads after the live source has changed.

use super::*;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop};

const NO_ROOM: &[u8] = b"#define EMPTY \nint ordinary_after_empty(void) { return 0; }\n#define ESCAPED\\\n /* comment */ \"tail\"\nint after(void) { return 0; }\n";

fn assert_generation(service: &FirstSliceService, receipt: &FirstSliceIndexReceipt) {
    let cancel = Cancellation::new();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 2);
    let file = document
        .files
        .iter()
        .find(|file| file.path == "limited.c")
        .unwrap();
    assert_eq!(file.language, "c");
    assert_eq!(file.byte_length, u64::try_from(NO_ROOM.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(NO_ROOM));
    let reference = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
        file.content_hash,
        None,
    );
    assert!(
        document
            .entities
            .iter()
            .filter(|entity| entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == file.id))
            .all(|entity| entity.kind == EntityKind::Module)
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.source.span().file() == file.id)
    );
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        let gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.domain == domain
                    && gap.detail == "c-preprocessor-replacement-not-analyzed"
                    && gap.source == reference
            })
            .collect::<Vec<_>>();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].reason, SkippedRegionReason::UnsupportedConstruct);
        assert!(
            document
                .coverage_records
                .iter()
                .any(|coverage| coverage.scope == CoverageScope::File(file.id)
                    && coverage.domain == domain
                    && coverage.status != CoverageStatus::Complete)
        );
    }
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "kept_neighbor"
                && entity.kind == EntityKind::Function)
    );
    let located = service
        .code_locate(
            receipt.generation,
            "limited.c".to_owned(),
            LocateMode::Exact,
            16,
            0,
            &cancel,
        )
        .unwrap();
    assert!(
        located
            .data
            .hits
            .iter()
            .any(|hit| hit.path == "limited.c" && hit.symbol.is_none())
    );
    let read = |reference: SourceRef, expected: &[u8]| {
        let result = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![reference.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &cancel,
            )
            .unwrap();
        assert_eq!(result.data.chunks.len(), 1);
        assert_eq!(result.data.chunks[0].reference, reference);
        assert_eq!(result.data.chunks[0].bytes, expected);
    };
    read(reference, NO_ROOM);
    let query = "tail";
    let found = service
        .find_literal(
            receipt.generation,
            file.id,
            query,
            None,
            SourceFindBudget::new(),
            &cancel,
        )
        .unwrap();
    assert_eq!(found.stop, SourceFindStop::EndOfSource);
    assert!(found.continuation.is_none());
    assert_eq!(found.matches.len(), 1);
    let start = u64::try_from(std::str::from_utf8(NO_ROOM).unwrap().find(query).unwrap()).unwrap();
    let expected = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, start, start + u64::try_from(query.len()).unwrap()).unwrap(),
        file.content_hash,
        None,
    );
    assert_eq!(found.matches[0], expected);
    read(expected, query.as_bytes());
}

#[test]
fn no_room_c_macro_keeps_file_gaps_exact_sources_and_analyzed_neighbor_after_reopen() {
    let fixture = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    fs::write(fixture.path().join("limited.c"), NO_ROOM).unwrap();
    fs::write(
        fixture.path().join("neighbor.c"),
        b"int kept_neighbor(void) { return 1; }\n",
    )
    .unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service = FirstSliceService::new_durable_with_policy(
        policy.clone(),
        paths.state_dir(),
        &Cancellation::new(),
    )
    .unwrap();
    let prepared = service
        .prepare_repository_with_options(
            fixture.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
            &Cancellation::new(),
        )
        .unwrap();
    let commit = service
        .publish_prepared_with_metrics(prepared, &Cancellation::new())
        .unwrap();
    let receipt = commit.receipt().clone();
    assert_eq!(receipt.indexed_files, 2);
    assert_generation(&service, &receipt);
    fs::write(
        fixture.path().join("limited.c"),
        b"unrelated live replacement\n",
    )
    .unwrap();
    assert_generation(&service, &receipt);
    drop(service);
    let reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &Cancellation::new())
            .unwrap();
    assert_generation(&reopened, &receipt);
}
