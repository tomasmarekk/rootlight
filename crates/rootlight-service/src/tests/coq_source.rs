//! Original Coq witnesses distinguish written headers from checked proofs.
//! The notation reference requires language-owned declaration-time evidence;
//! source retrieval remains exact even while custom notation is recovered.

use super::*;
use rootlight_ir::{ContainerRef, RelationEndpoint};

#[test]
fn original_coq_headers_binders_and_notation_reference_keep_exact_source() {
    let raw = include_bytes!("../../tests/fixtures/coq-squash/MRSquash.v");
    assert_eq!(raw.len(), 353);
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("written.v"), raw).unwrap();
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
        .find(|file| file.path == "written.v")
        .unwrap();
    assert_eq!(file.language, "coq");
    assert_eq!(file.byte_length, 353);
    assert_eq!(file.content_hash, content_hash(raw));
    let whole = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, 353).unwrap(),
        file.content_hash,
        None,
    );
    let exact = service
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
    assert_eq!(exact.data.chunks.len(), 1);
    assert_eq!(exact.data.chunks[0].reference, whole);
    assert_eq!(exact.data.chunks[0].bytes, raw);
    for (query, expected) in [
        ("map_squash", 1),
        ("map_squash__absent_source_control__", 0),
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
        for reference in found.matches {
            assert_eq!(reference.span().start_byte(), 134);
            assert_eq!(reference.span().end_byte(), 144);
            assert_eq!(reference.repository(), receipt.repository);
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.content_hash(), file.content_hash);
            let exact = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![reference.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(exact.data.chunks.len(), 1);
            assert_eq!(exact.data.chunks[0].reference, reference);
            assert_eq!(exact.data.chunks[0].bytes, query.as_bytes());
        }
    }
    let definition = |name: &str, kind, start, end| {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "coq"
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
                    && site.source.span().file() == file.id
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(sites.len(), 1, "exact definition {start}..{end}:{name}");
        let reference = &sites[0].source;
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), file.content_hash);
        let start = usize::try_from(start).expect("fixture start fits memory indexing");
        let end = usize::try_from(end).expect("fixture end fits memory indexing");
        assert_eq!(&raw[start..end], name.as_bytes());
        entity.id
    };
    let record = definition("squash", EntityKind::Struct, 9, 15);
    let constructor = definition("sq", EntityKind::Constructor, 37, 39);
    let lemma = definition("map_squash", EntityKind::Function, 134, 144);
    let parameter = definition("f", EntityKind::Parameter, 152, 153);
    for (subject, object, start, end) in
        [(record, constructor, 37, 39), (lemma, parameter, 152, 153)]
    {
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(subject)
                    && edge.object == RelationEndpoint::Entity(object)
                    && edge.evidence.source.as_ref().is_some_and(|source| {
                        source.span().file() == file.id
                            && source.span().start_byte() <= start
                            && source.span().end_byte() >= end
                    })
            })
            .collect();
        assert_eq!(edges.len(), 1, "typed authored Contains {start}..{end}");
        let reference = edges[0].evidence.source.as_ref().unwrap();
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), file.content_hash);
        assert_eq!(
            document
                .entities
                .iter()
                .find(|entity| entity.id == object)
                .unwrap()
                .container,
            Some(ContainerRef::Entity(subject))
        );
    }
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().file() == file.id
                && site.source.span().start_byte() == 79
                && site.source.span().end_byte() == 85
        })
        .collect();
    assert_eq!(references.len(), 1);
    assert_eq!(
        references[0].target,
        OccurrenceTarget::Resolved { symbol: record }
    );
    assert_eq!(references[0].source.repository(), receipt.repository);
    assert_eq!(references[0].source.generation(), receipt.generation);
    assert_eq!(references[0].source.content_hash(), file.content_hash);
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().file() == file.id
            && site.source.span().start_byte() == 79
            && site.source.span().end_byte() == 85
    }));
    for domain in [
        rootlight_ir::FactDomain::Entities,
        rootlight_ir::FactDomain::Occurrences,
        rootlight_ir::FactDomain::Relations,
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == domain && gap.source.span().file() == file.id)
        );
    }
}

#[test]
fn coq_notation_never_resolves_a_target_from_another_file() {
    let directory = TempDir::new().unwrap();
    let definition = "Definition target := 0.\n";
    let notation = "Notation \"! T\" := (target T).\n";
    fs::write(directory.path().join("definition.v"), definition).unwrap();
    fs::write(directory.path().join("notation.v"), notation).unwrap();
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
        .find(|file| file.path == "notation.v")
        .unwrap();
    let start = u64::try_from(notation.find("target").unwrap()).unwrap();
    let uses: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().file() == file.id
                && site.source.span().start_byte() == start
                && site.role == OccurrenceRole::Reference
        })
        .collect();
    assert_eq!(uses.len(), 1);
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo)
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.source.span().file() == file.id
                && gap.detail == "coq-declaration-time-notation-binding-unavailable")
    );
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
            vec![source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, source);
    assert_eq!(read.data.chunks[0].bytes, notation.as_bytes());
}

#[test]
fn encoded_coq_keeps_original_bytes_and_written_structure_without_notation_promotion() {
    let text = "\u{feff}Record résumé (A : Type) := crée { valeur : A }.\r\nNotation \"! T\" := (résumé T).\r\nLemma écrit (paramètre : nat) : True.\r\n";
    let raw: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("encoded.v"), &raw).unwrap();
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
        .find(|file| file.path == "encoded.v")
        .unwrap();
    assert_eq!(file.content_hash, content_hash(&raw));
    for (name, kind) in [
        ("résumé", EntityKind::Struct),
        ("écrit", EntityKind::Function),
        ("paramètre", EntityKind::Parameter),
    ] {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(entities.len(), 1);
        let start =
            u64::try_from(text[..text.find(name).unwrap()].encode_utf16().count() * 2).unwrap();
        let end = start + u64::try_from(name.encode_utf16().count() * 2).unwrap();
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| site.source.span().file() == file.id
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
                    && site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: entities[0].id
                        }))
                .count(),
            1
        );
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo)
    );
    let source = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        file.content_hash,
        None,
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(rootlight_source::SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, source);
    assert_eq!(read.data.chunks[0].bytes, raw);
}

#[test]
fn coq_competing_record_names_never_bind_an_earlier_definition_in_public_analysis() {
    for middle in [
        "Record holder := target { field : nat }.",
        "Record holder := make { target : nat }.",
        "Class holder := target { field : nat }.",
        "Class holder := make { target : nat }.",
        "Record holder := { field : nat }.",
        "Record holder `(x : C) := make { field : nat }.",
    ] {
        let text = format!("Definition target := 0. {middle} Notation \"! T\" := (target T).\n");
        let directory = TempDir::new().unwrap();
        fs::write(directory.path().join("written.v"), text.as_bytes()).unwrap();
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
            .find(|file| file.path == "written.v")
            .unwrap();
        assert_eq!(file.content_hash, content_hash(text.as_bytes()));
        let target = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Constant && entity.canonical_name == "target")
            .unwrap();
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == 11
                    && site.target == (OccurrenceTarget::Resolved { symbol: target.id }))
                .count(),
            1
        );
        let start = u64::try_from(text.rfind("target").unwrap()).unwrap();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().file() == file.id
                    && site.source.span().start_byte() == start
                    && site.role == OccurrenceRole::Reference
            })
            .collect();
        assert_eq!(sites.len(), 1, "{text}");
        assert_eq!(sites[0].source.span().end_byte(), start + 6);
        assert_eq!(sites[0].source.repository(), receipt.repository);
        assert_eq!(sites[0].source.generation(), receipt.generation);
        assert_eq!(sites[0].source.content_hash(), file.content_hash);
        assert!(
            matches!(sites[0].target, OccurrenceTarget::Unresolved { .. }),
            "{text}"
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo)
        );
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.source.span().start_byte() == start
                    && site.role == OccurrenceRole::Definition)
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().file() == file.id
                    && gap.detail == "coq-declaration-time-notation-binding-unavailable")
        );
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
                vec![source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, source);
        assert_eq!(read.data.chunks[0].bytes, text.as_bytes());
    }
}
