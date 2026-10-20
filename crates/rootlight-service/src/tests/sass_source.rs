//! Original Sass obligations survive service merge with exact physical reads.
//! File availability is independent of syntax, scope and binding limitations;
//! unsupported constructs retain explicit gaps instead of invented targets.

use super::*;
use rootlight_ir::{ContainerRef, RelationEndpoint};

#[test]
fn original_indented_sass_definitions_read_contains_and_source_are_exact() {
    let directory = TempDir::new().unwrap();
    let variables =
        include_bytes!("../../../rootlight-adapters/tests/fixtures/sass/_theme_variables.sass");
    let breadcrumbs =
        include_bytes!("../../../rootlight-adapters/tests/fixtures/sass/_theme_breadcrumbs.sass");
    assert_eq!(variables.len(), 2865);
    assert_eq!(breadcrumbs.len(), 604);
    fs::write(directory.path().join("variables.sass"), variables).unwrap();
    fs::write(directory.path().join("breadcrumbs.sass"), breadcrumbs).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let definition = |path: &str, name: &str, kind, start, end| {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "sass");
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "sass"
                    && entity.kind == kind
                    && entity.canonical_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .collect();
        assert_eq!(
            entities.len(),
            1,
            "{path}:{name} must have its authored kind"
        );
        let entity = entities[0];
        assert_eq!(entity.display_name, name);
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{path}:{name} exact definition leaf");
        let source = &definitions[0].source;
        assert_eq!(source.repository(), receipt.repository);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.span().file(), file.id);
        assert_eq!(source.content_hash(), file.content_hash);
        entity.id
    };
    let width = definition(
        "variables.sass",
        "$nav-content-width",
        EntityKind::Variable,
        334,
        352,
    );
    definition(
        "variables.sass",
        "$nav-media-query",
        EntityKind::Variable,
        378,
        394,
    );
    let rule = definition(
        "breadcrumbs.sass",
        ".breadcrumb-item::before",
        EntityKind::StyleRule,
        211,
        235,
    );
    let property = definition(
        "breadcrumbs.sass",
        "content",
        EntityKind::Property,
        238,
        245,
    );
    assert_eq!(
        document
            .entities
            .iter()
            .find(|entity| entity.id == property)
            .unwrap()
            .container,
        Some(ContainerRef::Entity(rule))
    );
    let contains: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(rule)
                && edge.object == RelationEndpoint::Entity(property)
        })
        .collect();
    assert_eq!(contains.len(), 1);
    let source = contains[0].evidence.source.as_ref().unwrap();
    assert!(source.span().start_byte() <= 238 && source.span().end_byte() >= 245);
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(property)
                && edge.object == RelationEndpoint::Entity(rule))
    );
    let file = document
        .files
        .iter()
        .find(|file| file.path == "variables.sass")
        .unwrap();
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().file() == file.id
                && site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 437
                && site.source.span().end_byte() == 455
        })
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved { symbol: width }
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.source.span().file() == file.id
                && site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 437
                && site.source.span().end_byte() == 455)
    );
    for (path, raw) in [
        ("variables.sass", variables.as_slice()),
        ("breadcrumbs.sass", breadcrumbs.as_slice()),
    ] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.content_hash, content_hash(raw));
        let whole = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
            file.content_hash,
            None,
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
        if path == "breadcrumbs.sass" {
            for (query, expected) in [
                (".breadcrumb-item::before", 1),
                ("__absent_sass_source_87673c00f5e749848ef8499135f4c328__", 0),
            ] {
                let found = service
                    .find_literal(
                        receipt.generation,
                        file.id,
                        query,
                        None,
                        rootlight_source::SourceFindBudget::new(),
                        &deadline(),
                    )
                    .unwrap();
                assert_eq!(found.stop, rootlight_source::SourceFindStop::EndOfSource);
                assert!(found.continuation.is_none());
                assert_eq!(found.matches.len(), expected);
                for source in found.matches {
                    assert_eq!(
                        (source.span().start_byte(), source.span().end_byte()),
                        (211, 235)
                    );
                    assert_eq!(source.content_hash(), file.content_hash);
                    let read = service
                        .source_read_with_options_and_budget(
                            receipt.generation,
                            vec![source.clone()],
                            SourceReadOptions::new()
                                .with_context_lines_before(0)
                                .with_context_lines_after(0),
                            FirstSliceBudget::default(),
                            &deadline(),
                        )
                        .unwrap();
                    assert_eq!(read.data.chunks[0].reference, source);
                    assert_eq!(read.data.chunks[0].bytes, query.as_bytes());
                }
            }
        }
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.as_str().starts_with("sass-"))
    );
}

#[test]
fn rejected_sass_indentation_remains_available_without_false_definitions() {
    for (path, raw) in [
        ("leading.sass", b"  $value: red\n".as_slice()),
        ("mixed.sass", b".outer\n \tcolor: red\n".as_slice()),
        (
            "switched.sass",
            b".outer\n\tcolor: red\n.after\n  color: blue\n".as_slice(),
        ),
        ("broken.sass", b"+missing(\n".as_slice()),
    ] {
        let directory = TempDir::new().unwrap();
        fs::write(directory.path().join(path), raw).unwrap();
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
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "sass");
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.language == "sass")
        );
        assert!(!document.skipped_regions.is_empty());
        assert!(!document.occurrences.iter().any(|site| site.file == file.id));
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap();
        assert_eq!(coverage.status, CoverageStatus::Unknown);
        for domain in [
            rootlight_ir::FactDomain::Entities,
            rootlight_ir::FactDomain::Occurrences,
            rootlight_ir::FactDomain::Relations,
        ] {
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.domain == domain
                    && gap.source.span().file() == file.id
                    && matches!(
                        gap.detail.as_str(),
                        "sass-initial-indentation-analysis-unavailable"
                            | "sass-invalid-indentation"
                            | "sass-native-parse-failed"
                    )
            }));
        }
        let source = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
            file.content_hash,
            None,
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![source],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, raw);
    }
}

#[test]
fn same_spelling_in_another_file_never_promotes_an_unbound_sass_read() {
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("definition.sass"), b"$outside: red\n").unwrap();
    fs::write(
        directory.path().join("consumer.sass"),
        b".item\n  color: $outside\n",
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
    assert!(
        document.entities.iter().any(
            |entity| entity.kind == EntityKind::Variable && entity.canonical_name == "$outside"
        )
    );
    let consumer = document
        .files
        .iter()
        .find(|file| file.path == "consumer.sass")
        .unwrap();
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().file() == consumer.id && site.role == OccurrenceRole::Read
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn queued_utf16_analysis_keeps_exact_original_physical_source_readable() {
    let directory = TempDir::new().unwrap();
    let text = "$色: red\r\n.item\r\n  color: $色\r\n";
    let mut raw = vec![0xff, 0xfe];
    for unit in text.encode_utf16() {
        raw.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(directory.path().join("encoded.sass"), &raw).unwrap();
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
        .find(|file| file.path == "encoded.sass")
        .unwrap();
    assert_eq!(file.language, "sass");
    assert_eq!(file.content_hash, content_hash(&raw));
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.language == "sass")
    );
    assert!(!document.skipped_regions.is_empty());
    let source = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
        file.content_hash,
        None,
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![source],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(rootlight_source::SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, raw);
}
