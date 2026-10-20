//! The authored Flux witness qualifies source access separately from analysis.
//! Kinds, leaves and native parameter targets are mandatory even in Tier D.

use super::*;
use rootlight_ir::{ContainerRef, RelationEndpoint};

#[test]
fn original_flux_functions_parameters_reads_and_source_are_exact() {
    let directory = TempDir::new().unwrap();
    let raw =
        include_bytes!("../../../rootlight-adapters/tests/fixtures/flux/original-scale_test.flux");
    assert_eq!(raw.len(), 815);
    fs::write(directory.path().join("original.flux"), raw).unwrap();
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
        .find(|file| file.path == "original.flux")
        .unwrap();
    assert_eq!(file.content_hash, content_hash(raw));
    assert_eq!(file.byte_length, 815);
    assert_eq!(file.language, "flux");
    let definition = |name: &str, kind, start, end| {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "flux"
                    && entity.kind == kind
                    && entity.canonical_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .collect();
        assert_eq!(entities.len(), 1, "authored {kind:?}:{name}");
        let entity = entities[0];
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(sites.len(), 1, "exact definition {name}");
        let source = &sites[0].source;
        assert_eq!(source.repository(), receipt.repository);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.content_hash(), file.content_hash);
        assert_eq!(source.span().file(), file.id);
        let physical = usize::try_from(start).unwrap()..usize::try_from(end).unwrap();
        assert_eq!(&raw[physical], name.as_bytes());
        entity.id
    };
    let function = definition("scaleTime", EntityKind::Function, 117, 126);
    let d = definition("d", EntityKind::Parameter, 130, 131);
    let n = definition("n", EntityKind::Parameter, 133, 134);
    for parameter in [d, n] {
        assert_eq!(
            document
                .entities
                .iter()
                .find(|entity| entity.id == parameter)
                .unwrap()
                .container,
            Some(ContainerRef::Entity(function))
        );
    }
    let contains: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(function)
                && edge.object == RelationEndpoint::Entity(d)
        })
        .collect();
    assert_eq!(contains.len(), 1);
    let source = contains[0].evidence.source.as_ref().unwrap();
    assert!(source.span().start_byte() <= 130 && source.span().end_byte() >= 131);
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(d)
                && edge.object == RelationEndpoint::Entity(function))
    );
    for (start, end, target) in [(176, 177, d), (182, 183, n)] {
        let reads: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.file == file.id
                    && site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(reads.len(), 1);
        assert_eq!(
            reads[0].target,
            OccurrenceTarget::Resolved { symbol: target }
        );
        assert_eq!(reads[0].enclosing, Some(function));
        assert_eq!(reads[0].source.content_hash(), file.content_hash);
    }
    assert!(!document.occurrences.iter().any(|site| site.file == file.id
        && site.role == OccurrenceRole::Definition
        && site.source.span().start_byte() == 285
        && site.source.span().end_byte() == 294));
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
    for (needle, ranges) in [
        (
            "scaleTime",
            vec![(117, 126), (285, 294), (336, 345), (387, 396), (438, 447)],
        ),
        (
            "__absent_flux_843f8282f1cd46c7b87dce7bffceaa20__",
            Vec::new(),
        ),
    ] {
        let found = service
            .find_literal(
                receipt.generation,
                file.id,
                needle,
                None,
                rootlight_source::SourceFindBudget::new(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(found.stop, rootlight_source::SourceFindStop::EndOfSource);
        assert!(found.continuation.is_none());
        assert_eq!(found.matches.len(), ranges.len());
        for (source, (start, end)) in found.matches.into_iter().zip(ranges) {
            assert_eq!(
                (source.span().start_byte(), source.span().end_byte()),
                (start, end)
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
            assert_eq!(read.data.chunks[0].bytes, needle.as_bytes());
        }
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.as_str().starts_with("flux-"))
    );
}

#[test]
fn unsupported_flux_validation_retains_exact_bytes_and_unknown_domains() {
    for (text, reason) in [
        ("value = /[a-z]+/\n", "flux-regex-validation-unavailable"),
        (
            "value = 2020-01-01T00:00:00.12345678901234567890Z\n",
            "flux-time-precision-unavailable",
        ),
        (
            "broken = (d) => date.scale(d: \n",
            "flux-native-parse-recovery",
        ),
    ] {
        let directory = TempDir::new().unwrap();
        fs::write(directory.path().join("unsupported.flux"), text).unwrap();
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
            .find(|file| file.path == "unsupported.flux")
            .unwrap();
        assert_eq!(file.language, "flux");
        assert_eq!(file.content_hash, content_hash(text.as_bytes()));
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.language == "flux")
        );
        assert!(!document.occurrences.iter().any(|site| site.file == file.id));
        for domain in [
            rootlight_ir::FactDomain::Entities,
            rootlight_ir::FactDomain::Occurrences,
            rootlight_ir::FactDomain::Relations,
        ] {
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.domain == domain
                        && gap.source.span().file() == file.id
                        && gap.detail.as_str() == reason
                        && gap.source.span().start_byte() == 0
                        && gap.source.span().end_byte() == file.byte_length)
            );
        }
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap();
        assert_eq!(coverage.status, CoverageStatus::Unknown);
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
        assert_eq!(read.data.chunks[0].reference, whole);
        assert_eq!(read.data.chunks[0].bytes, text.as_bytes());
        let literal = service
            .find_literal(
                receipt.generation,
                file.id,
                "value",
                None,
                rootlight_source::SourceFindBudget::new(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(literal.stop, rootlight_source::SourceFindStop::EndOfSource);
        assert!(literal.continuation.is_none());
        assert_eq!(
            literal.matches.len(),
            usize::from(text.starts_with("value"))
        );
    }
}

#[test]
fn flux_formal_binding_never_crosses_files_or_unknown_source_suffixes() {
    let directory = TempDir::new().unwrap();
    for (path, source) in [
        ("first.flux", "one = (value) => value\n"),
        ("second.flux", "two = () => value\n"),
        ("unclaimed.fluxish", "three = (value) => value\n"),
    ] {
        fs::write(directory.path().join(path), source).unwrap();
    }
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let second = document
        .files
        .iter()
        .find(|file| file.path == "second.flux")
        .unwrap();
    let unknown = document
        .files
        .iter()
        .find(|file| file.path == "unclaimed.fluxish")
        .unwrap();
    assert_eq!(second.language, "flux");
    assert_ne!(unknown.language, "flux");
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.file == second.id
                && site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 12
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(!document.entities.iter().any(|entity| {
        entity.language == "flux"
            && entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == unknown.id)
    }));
}

#[path = "flux_source/depth_decline.rs"]
mod depth_decline;

#[path = "flux_source/audit_depth_decline.rs"]
mod audit_depth_decline;
