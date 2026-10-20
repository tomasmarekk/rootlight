//! The actual parsed-but-audit-declined file keeps its source and a healthy sibling.

use super::*;

#[test]
fn original_flux_ast_audit_depth_keeps_source_unknown_and_healthy_sibling() {
    let raw = include_bytes!(
        "../../../../rootlight-adapters/tests/fixtures/flux/original-stack_overflow_2.flux"
    );
    assert_eq!(raw.len(), 1757);
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("audit.flux"), raw).unwrap();
    let ordinary = b"measure = (value) => consume(v: value)\n";
    fs::write(directory.path().join("ordinary.flux"), ordinary).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "audit.flux")
        .unwrap();
    let healthy = document
        .files
        .iter()
        .find(|file| file.path == "ordinary.flux")
        .unwrap();
    assert_eq!(file.language, "flux");
    assert_eq!(file.content_hash, content_hash(raw));
    assert_eq!(file.byte_length, u64::try_from(raw.len()).unwrap());
    let whole = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
        file.content_hash,
        None,
    );
    for domain in [
        rootlight_ir::FactDomain::Entities,
        rootlight_ir::FactDomain::Occurrences,
        rootlight_ir::FactDomain::Relations,
    ] {
        let gaps: Vec<_> = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.domain == domain
                    && gap.detail == "flux-native-depth-unavailable"
                    && gap.source.span().file() == file.id
            })
            .collect();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].source, whole);
    }
    assert!(document.diagnostics.iter().any(|diagnostic| diagnostic.code
        == "flux-native-depth-unavailable"
        && diagnostic.source.as_ref() == Some(&whole)
        && diagnostic.coverage_effect == CoverageStatus::Unknown));
    assert!(!document.entities.iter().any(|entity| {
        entity
            .evidence
            .source
            .as_ref()
            .is_some_and(|source| source.span().file() == file.id)
    }));
    assert!(!document.occurrences.iter().any(|site| site.file == file.id));
    assert_eq!(
        service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap()
            .status,
        CoverageStatus::Unknown
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![whole.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, whole);
    assert_eq!(read.data.chunks[0].bytes, raw);
    for (needle, count) in [("false", 195), ("__absent_audit_depth_34a71c9f__", 0)] {
        let expected: Vec<_> = raw
            .windows(needle.len())
            .enumerate()
            .filter_map(|(start, bytes)| (bytes == needle.as_bytes()).then_some(start))
            .collect();
        assert_eq!(expected.len(), count);
        let mut cursor = None;
        let mut matches = Vec::new();
        let mut last = 0;
        loop {
            let found = service
                .find_literal(
                    receipt.generation,
                    file.id,
                    needle,
                    cursor,
                    rootlight_source::SourceFindBudget::new(),
                    &deadline(),
                )
                .unwrap();
            matches.extend(found.matches);
            assert!(matches.len() <= expected.len());
            if let Some(next) = found.continuation {
                assert_eq!(found.stop, rootlight_source::SourceFindStop::MatchLimit);
                assert!(next.next_byte > last);
                last = next.next_byte;
                cursor = Some(next);
            } else {
                assert_eq!(found.stop, rootlight_source::SourceFindStop::EndOfSource);
                break;
            }
        }
        assert_eq!(matches.len(), expected.len());
        for (source, start) in matches.iter().zip(expected) {
            assert_eq!(source.repository(), receipt.repository);
            assert_eq!(source.generation(), receipt.generation);
            assert_eq!(source.content_hash(), file.content_hash);
            assert_eq!(
                source.span(),
                SourceSpan::new(
                    file.id,
                    u64::try_from(start).unwrap(),
                    u64::try_from(start + needle.len()).unwrap()
                )
                .unwrap()
            );
        }
    }
    let functions: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Function
                && entity.canonical_name == "measure"
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == healthy.id)
        })
        .collect();
    let formals: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == "value"
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == healthy.id)
        })
        .collect();
    assert_eq!(functions.len(), 1);
    assert_eq!(formals.len(), 1);
    assert_eq!(healthy.content_hash, content_hash(ordinary));
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.file == healthy.id
                && site.role == OccurrenceRole::Read
                && site.enclosing == Some(functions[0].id)
                && site.target
                    == (OccurrenceTarget::Resolved {
                        symbol: formals[0].id
                    })
                && site.source.content_hash() == healthy.content_hash)
    );
}
