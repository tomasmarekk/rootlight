//! A native CPP projection refusal preserves publication and exact source access.
//! The unsupported file remains explicit beside an independently analyzed file,
//! including durable reads after the live source has changed.

use super::*;
use rootlight_ir::OccurrenceTarget;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop};

const NO_ROOM: &[u8] = b"#define EMPTY \nint ordinary_after_empty(void) { return 0; }\n#define ESCAPED\\\n /* comment */ \"tail\"\nint after(void) { return 0; }\n";

const NEIGHBOR: &[u8] = b"\xef\xbb\xbf#define CPP_NATIVE \"prefix\"\n#define CPP_RAW R\"tag( // FAKE int phantom(void); )tag\"\nint kept_neighbor(void) { return 1; }\n";

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
        .find(|file| file.path == "limited.cpp")
        .unwrap();
    assert_eq!(file.language, "cpp");
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
                    && gap.detail == "cpp-preprocessor-replacement-not-analyzed"
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
            "limited.cpp".to_owned(),
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
            .any(|hit| hit.path == "limited.cpp" && hit.symbol.is_none())
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
    let neighbor = document
        .files
        .iter()
        .find(|file| file.path == "neighbor.cpp")
        .unwrap();
    assert_eq!(neighbor.language, "cpp");
    assert_eq!(neighbor.byte_length, u64::try_from(NEIGHBOR.len()).unwrap());
    assert_eq!(neighbor.content_hash, content_hash(NEIGHBOR));
    let whole_neighbor = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(neighbor.id, 0, neighbor.byte_length).unwrap(),
        neighbor.content_hash,
        None,
    );
    read(whole_neighbor, NEIGHBOR);
    for name in ["CPP_NATIVE", "CPP_RAW", "kept_neighbor"] {
        let matches = document
            .entities
            .iter()
            .filter(|entity| {
                entity.canonical_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == neighbor.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "missing or duplicated CPP definition: {name}"
        );
        let owner = matches[0].evidence.source.as_ref().unwrap().clone();
        let expected_owner: &[u8] = match name {
            "CPP_NATIVE" => b"#define CPP_NATIVE \"prefix\"\n",
            "CPP_RAW" => b"#define CPP_RAW R\"tag( // FAKE int phantom(void); )tag\"\n",
            "kept_neighbor" => b"int kept_neighbor(void) { return 1; }",
            _ => unreachable!(),
        };
        let owner_start = NEIGHBOR
            .windows(expected_owner.len())
            .position(|bytes| bytes == expected_owner)
            .unwrap();
        let expected_owner_ref = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                neighbor.id,
                u64::try_from(owner_start).unwrap(),
                u64::try_from(owner_start + expected_owner.len()).unwrap(),
            )
            .unwrap(),
            neighbor.content_hash,
            None,
        );
        assert_eq!(owner, expected_owner_ref);
        read(owner.clone(), expected_owner);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.file == neighbor.id
                    && site.role == OccurrenceRole::Definition
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: matches[0].id,
                        }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            definitions.len(),
            1,
            "missing or duplicated CPP definition occurrence: {name}"
        );
        let definition = definitions[0].source.clone();
        let span = definition.span();
        assert!(
            owner.span().start_byte() <= span.start_byte()
                && owner.span().end_byte() >= span.end_byte()
        );
        assert_eq!(
            &NEIGHBOR[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name.as_bytes()
        );
        read(definition, name.as_bytes());
        let located = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
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
                .any(|hit| hit.path == "neighbor.cpp" && hit.symbol == Some(matches[0].id))
        );
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| matches!(entity.canonical_name.as_str(), "FAKE" | "phantom"))
    );
    let literal = service
        .find_literal(
            receipt.generation,
            neighbor.id,
            "phantom",
            None,
            SourceFindBudget::new(),
            &cancel,
        )
        .unwrap();
    assert_eq!(literal.stop, SourceFindStop::EndOfSource);
    assert!(literal.continuation.is_none());
    assert_eq!(literal.matches.len(), 1);
    let start = NEIGHBOR
        .windows(b"phantom".len())
        .position(|bytes| bytes == b"phantom")
        .unwrap();
    let expected = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(
            neighbor.id,
            u64::try_from(start).unwrap(),
            u64::try_from(start + b"phantom".len()).unwrap(),
        )
        .unwrap(),
        neighbor.content_hash,
        None,
    );
    assert_eq!(literal.matches[0], expected);
    read(expected, b"phantom");
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
fn cpp_macro_gaps_and_raw_literal_text_keep_exact_sources_after_reopen() {
    let fixture = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    fs::write(fixture.path().join("limited.cpp"), NO_ROOM).unwrap();
    fs::write(fixture.path().join("neighbor.cpp"), NEIGHBOR).unwrap();
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
        fixture.path().join("limited.cpp"),
        b"unrelated live replacement\n",
    )
    .unwrap();
    fs::write(
        fixture.path().join("neighbor.cpp"),
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
