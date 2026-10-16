//! Physical patch sections and documentary rows survive the public query lifecycle.
//! Target program meaning remains explicitly unavailable while original bytes,
//! Unicode offsets and both durable source layouts remain independently readable.

use super::*;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop};

const PATCH: &str = concat!(
    "diff --git a/CMakeLists.txt b/CMakeLists.txt\r\n",
    "index 1111111..2222222 100644\r\n",
    "--- a/CMakeLists.txt\r\n",
    "+++ b/CMakeLists.txt\r\n",
    "@@ -1,3 +1,3 @@ configure λ😀\r\n",
    " set(unchanged retained)\r\n",
    "-set(legacy_value before)\r\n",
    "+set(new_value after)\r\n",
    " message(\"physicalrowmarker λ😀\")\r\n",
    "@@ -9 +9 @@ footer\r\n",
    "-# old tail\r\n",
    "+# new tail\r\n",
);

fn exact_read(service: &FirstSliceService, reference: &SourceRef, expected: &[u8]) {
    let result = service
        .source_read_with_options_and_budget(
            reference.generation(),
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(result.data.chunks.len(), 1);
    assert_eq!(result.data.chunks[0].reference, *reference);
    assert_eq!(result.data.chunks[0].bytes, expected);
}

fn assert_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
) -> BTreeSet<SymbolId> {
    let cancellation = Cancellation::new();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "review.cmake")
        .unwrap();
    assert_eq!(file.language, "cmake");
    assert_eq!(file.encoding, "utf-8");
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    let whole = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
        file.content_hash,
        None,
    );
    exact_read(service, &whole, source.as_bytes());
    let entities = document
        .entities
        .iter()
        .filter(|entity| {
            entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|reference| reference.span().file() == file.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(entities.len(), 4);
    assert!(entities.iter().all(|entity| {
        entity.language == "diff"
            && matches!(
                entity.kind,
                EntityKind::Module | EntityKind::DocumentSection
            )
    }));
    let sections = entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::DocumentSection)
        .collect::<Vec<_>>();
    assert_eq!(sections.len(), 3);
    let expected_headings = [
        "diff --git a/CMakeLists.txt b/CMakeLists.txt",
        "@@ -1,3 +1,3 @@ configure λ😀",
        "@@ -9 +9 @@ footer",
    ];
    assert_eq!(
        sections
            .iter()
            .map(|entity| entity.canonical_name.as_str())
            .collect::<BTreeSet<_>>(),
        expected_headings.into_iter().collect(),
    );
    let mut offset = 0_u64;
    let expected_rows = source
        .split_inclusive('\n')
        .map(|row| {
            let start = offset;
            offset += u64::try_from(row.len()).unwrap();
            (start, offset)
        })
        .collect::<BTreeSet<_>>();
    let mut observed_rows = BTreeSet::new();
    let mut row_roles = BTreeSet::new();
    for section in sections {
        let located = service
            .code_locate(
                receipt.generation,
                section.canonical_name.clone(),
                LocateMode::Exact,
                16,
                0,
                &cancellation,
            )
            .unwrap();
        assert!(!located.data.truncated);
        assert!(located.data.hits.iter().any(|hit| {
            hit.symbol == Some(section.id)
                && hit.kind == "document_section"
                && hit.language == "diff"
                && hit.path == "review.cmake"
        }));
        let explained = service
            .symbol_explain(receipt.generation, section.id, &cancellation)
            .unwrap();
        assert!(!explained.data.truncated);
        assert_eq!(explained.data.entity, **section);
        let signature = explained.data.signature.as_ref().unwrap();
        assert_eq!(signature.text(), section.canonical_name);
        assert!(!signature.is_truncated());
        signature
            .verify_source_bytes(section.canonical_name.as_bytes())
            .unwrap();
        let definitions = explained
            .data
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: section.id }
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        let start = u64::try_from(source.find(&section.canonical_name).unwrap()).unwrap();
        let heading = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                start,
                start + u64::try_from(section.canonical_name.len()).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        );
        assert_eq!(definitions[0].source, heading);
        assert_eq!(
            definitions[0].syntactic_text_hash,
            content_hash(section.canonical_name.as_bytes())
        );
        exact_read(service, &heading, section.canonical_name.as_bytes());
        for row in explained.data.occurrences.iter().filter(|occurrence| {
            occurrence.role == OccurrenceRole::Documentation
                && occurrence.target == OccurrenceTarget::Resolved { symbol: section.id }
        }) {
            assert_eq!(row.source.repository(), receipt.repository);
            assert_eq!(row.source.generation(), receipt.generation);
            assert_eq!(row.source.content_hash(), file.content_hash);
            assert_eq!(row.source.span().file(), file.id);
            let span = (row.source.span().start_byte(), row.source.span().end_byte());
            assert!(expected_rows.contains(&span));
            assert!(observed_rows.insert(span));
            row_roles.insert(row.syntax_kind.clone());
            let bytes = &source.as_bytes()
                [usize::try_from(span.0).unwrap()..usize::try_from(span.1).unwrap()];
            assert!(bytes.ends_with(b"\r\n"));
            assert_eq!(row.syntactic_text_hash, content_hash(bytes));
            exact_read(service, &row.source, bytes);
        }
    }
    assert_eq!(observed_rows, expected_rows);
    assert_eq!(
        row_roles,
        BTreeSet::from(
            [
                "patch.metadata_row.documentation",
                "patch.context_row.documentation",
                "patch.deleted_row.documentation",
                "patch.added_row.documentation",
            ]
            .map(str::to_owned)
        )
    );
    assert!(
        document
            .occurrences
            .iter()
            .filter(|site| site.source.span().file() == file.id)
            .all(|site| {
                matches!(
                    site.role,
                    OccurrenceRole::Definition | OccurrenceRole::Documentation
                )
            })
    );
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.domain == domain
                && gap.reason == SkippedRegionReason::UnsupportedConstruct
                && gap.detail == "patch-target-fragment-analysis-and-applicability-unavailable"
                && gap.source == whole
        }));
        assert!(document.coverage_records.iter().any(|coverage| {
            coverage.scope == CoverageScope::File(file.id)
                && coverage.domain == domain
                && coverage.status != CoverageStatus::Complete
        }));
    }
    for query in ["legacy_value", "new_value", "physicalrowmarker λ😀"] {
        let found = service
            .find_literal(
                receipt.generation,
                file.id,
                query,
                None,
                SourceFindBudget::new(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(found.stop, SourceFindStop::EndOfSource);
        assert!(found.continuation.is_none());
        assert_eq!(found.matches.len(), 1);
        let start = u64::try_from(source.find(query).unwrap()).unwrap();
        let expected = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, start, start + u64::try_from(query.len()).unwrap()).unwrap(),
            file.content_hash,
            None,
        );
        assert_eq!(found.matches[0], expected);
        exact_read(service, &expected, query.as_bytes());
        let located = service
            .code_locate(
                receipt.generation,
                query.to_owned(),
                LocateMode::Exact,
                16,
                0,
                &cancellation,
            )
            .unwrap();
        assert!(located.data.hits.iter().all(|hit| hit.symbol.is_none()));
    }
    entities.into_iter().map(|entity| entity.id).collect()
}

fn index(service: &mut FirstSliceService, root: &Path) -> FirstSliceIndexCommit {
    let cancellation = Cancellation::new();
    let prepared = service
        .prepare_repository_with_options(
            root,
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
            &cancellation,
        )
        .unwrap();
    service
        .publish_prepared_with_metrics(prepared, &cancellation)
        .unwrap()
}

fn lifecycle(extra_files: usize) {
    let fixture = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    fs::write(fixture.path().join("review.cmake"), PATCH).unwrap();
    for ordinal in 0..extra_files {
        fs::write(fixture.path().join(format!("empty-{ordinal}.txt")), b"").unwrap();
    }
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service = FirstSliceService::new_durable_with_policy(
        policy.clone(),
        paths.state_dir(),
        &Cancellation::new(),
    )
    .unwrap();
    let cold = index(&mut service, fixture.path());
    assert_eq!(cold.evidence().rebuilt_files, 1);
    let first = cold.receipt().clone();
    let symbols = assert_generation(&service, &first, PATCH);
    let noop = index(&mut service, fixture.path());
    assert_eq!(noop.receipt(), &first);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    let changed = PATCH.replace("new_value after", "new_value updated");
    fs::write(fixture.path().join("review.cmake"), &changed).unwrap();
    let update = index(&mut service, fixture.path());
    assert_eq!(update.evidence().rebuilt_files, 1);
    let second = update.receipt().clone();
    assert_ne!(second.generation, first.generation);
    assert_eq!(symbols, assert_generation(&service, &second, &changed));
    let noop = index(&mut service, fixture.path());
    assert_eq!(noop.receipt(), &second);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    for receipt in [&first, &second] {
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(
                paths
                    .state_dir()
                    .join("first-slice/repositories")
                    .join(receipt.repository.to_string())
                    .join(receipt.generation.to_string())
                    .join("manifest.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest["source_storage"]["version"],
            if extra_files == 0 { 1 } else { 2 }
        );
    }
    drop(service);
    fs::write(
        fixture.path().join("review.cmake"),
        b"unrelated live bytes\n",
    )
    .unwrap();
    let reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &Cancellation::new())
            .unwrap();
    assert_eq!(symbols, assert_generation(&reopened, &first, PATCH));
    assert_eq!(symbols, assert_generation(&reopened, &second, &changed));
}

#[test]
fn cmake_patch_blob_generations_keep_physical_document_evidence() {
    lifecycle(0);
}

#[test]
fn cmake_patch_packed_generations_keep_physical_document_evidence() {
    lifecycle(256);
}
