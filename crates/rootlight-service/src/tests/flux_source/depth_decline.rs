//! A declined original Flux file must not block safe repository source access.
//! Another ordinary file retains its independently checked structural witness.

use super::*;

#[test]
fn original_deep_flux_keeps_exact_source_and_ordinary_file_analysis() {
    let raw = include_bytes!(
        "../../../../rootlight-adapters/tests/fixtures/flux/original-stack_overflow.flux"
    );
    assert_eq!(raw.len(), 7717);
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("deep.flux"), raw).unwrap();
    fs::write(
        directory.path().join("ordinary.flux"),
        b"measure = (value) => consume(v: value)\n",
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let deep = document
        .files
        .iter()
        .find(|file| file.path == "deep.flux")
        .unwrap();
    let ordinary = document
        .files
        .iter()
        .find(|file| file.path == "ordinary.flux")
        .unwrap();
    assert_eq!(deep.language, "flux");
    assert_eq!(deep.content_hash, content_hash(raw));
    assert_eq!(deep.byte_length, raw.len() as u64);
    let whole = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(deep.id, 0, deep.byte_length).unwrap(),
        deep.content_hash,
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
                    && gap.source.span().file() == deep.id
                    && gap.detail == "flux-native-depth-unavailable"
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
            .is_some_and(|source| source.span().file() == deep.id)
    }));
    assert!(!document.occurrences.iter().any(|site| site.file == deep.id));
    assert_eq!(
        service
            .source_file_coverage_until(receipt.generation, deep.id, &deadline())
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
    for needle in ["|> range(start:", "__absent_native_depth_0ddbe5b5__"] {
        let expected: Vec<_> = raw
            .windows(needle.len())
            .enumerate()
            .filter_map(|(start, bytes)| (bytes == needle.as_bytes()).then_some(start))
            .collect();
        assert_eq!(
            expected.len(),
            if needle.starts_with("|>") { 282 } else { 0 }
        );
        let mut cursor = None;
        let mut matches = Vec::new();
        let mut last_byte = 0;
        loop {
            let found = service
                .find_literal(
                    receipt.generation,
                    deep.id,
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
                assert!(next.next_byte > last_byte);
                last_byte = next.next_byte;
                cursor = Some(next);
            } else {
                assert_eq!(found.stop, rootlight_source::SourceFindStop::EndOfSource);
                break;
            }
        }
        assert_eq!(matches.len(), expected.len());
        for (source, start) in matches.iter().zip(expected) {
            assert_eq!(source.content_hash(), deep.content_hash);
            assert_eq!(
                source.span(),
                SourceSpan::new(deep.id, start as u64, (start + needle.len()) as u64).unwrap()
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
                    .is_some_and(|source| source.span().file() == ordinary.id)
        })
        .collect();
    let parameters: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == "value"
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == ordinary.id)
        })
        .collect();
    assert_eq!(functions.len(), 1);
    assert_eq!(parameters.len(), 1);
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.file == ordinary.id
                && site.role == OccurrenceRole::Read
                && site.target
                    == (OccurrenceTarget::Resolved {
                        symbol: parameters[0].id
                    })
                && site.enclosing == Some(functions[0].id))
    );
}
