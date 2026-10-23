//! Written Eiffel spelling must not become generic lexical binding evidence.

use super::*;
use rootlight_ir::{ContainerRef, OccurrenceTarget};
use std::fs;

#[test]
fn eiffel_unproven_targets_stay_unresolved_after_publication_and_no_op() {
    let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let inputs = [
        (
            "client.e",
            "class CLIENT\nfeature\n field: TARGET\n run (target: TARGET)\n do\n target.run (1)\n end\nend\n",
        ),
        (
            "target.e",
            "class TARGET\nfeature\n run (value: INTEGER) do end\nend\n",
        ),
    ];
    for (path, source) in inputs {
        fs::write(fixture.path().join(path), source).unwrap();
    }
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    let document = service
        .inspect_generation(
            receipt.repository,
            Some(receipt.generation),
            &cancellation,
            |snapshot| snapshot.document().clone(),
        )
        .unwrap();
    assert_eq!(document.files.len(), inputs.len());
    for syntax in [
        "eiffel.identifier.reference",
        "eiffel.type.reference",
        "eiffel.written_call.call",
    ] {
        let occurrences: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.syntax_kind == syntax)
            .collect();
        assert!(!occurrences.is_empty(), "missing written {syntax}");
        assert!(
            occurrences
                .iter()
                .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })),
            "unsupported language binding was promoted: {occurrences:#?}"
        );
    }
    let client = document
        .files
        .iter()
        .find(|file| file.path == "client.e")
        .unwrap();
    let original = inputs[0].1;
    let physical_hash = content_hash(original.as_bytes());
    assert_eq!(client.repository, receipt.repository);
    assert_eq!(client.generation, receipt.generation);
    assert_eq!(client.content_hash, physical_hash);
    assert_eq!(client.byte_length, u64::try_from(original.len()).unwrap());
    let entity = |name: &str, kind: EntityKind, container: ContainerRef| {
        let found: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.canonical_name == name
                    && entity.kind == kind
                    && entity.container == Some(container)
            })
            .collect();
        assert_eq!(found.len(), 1, "unique current entity {name}");
        found[0]
    };
    let module = entity(
        "client.e",
        EntityKind::Module,
        ContainerRef::File(client.id),
    );
    let class = entity("CLIENT", EntityKind::Class, ContainerRef::Entity(module.id));
    let routine = entity("run", EntityKind::Function, ContainerRef::Entity(class.id));
    let parameter = entity(
        "target",
        EntityKind::Parameter,
        ContainerRef::Entity(routine.id),
    );
    assert_eq!(
        module.evidence.source.as_ref().unwrap().span(),
        SourceSpan::new(client.id, 0, client.byte_length).unwrap()
    );
    for entity in [module, class, routine, parameter] {
        let source = entity.evidence.source.as_ref().unwrap();
        assert_eq!(entity.repository, receipt.repository);
        assert_eq!(entity.generation, receipt.generation);
        assert_eq!(entity.provenance, module.provenance);
        assert_eq!(source.repository(), receipt.repository);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.span().file(), client.id);
        assert_eq!(source.content_hash(), physical_hash);
    }
    for (owner, child) in [(module, class), (class, routine), (routine, parameter)] {
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(owner.id)
                        && edge.object == RelationEndpoint::Entity(child.id)
                })
                .count(),
            1
        );
    }
    for (entity, literal, spelling) in [
        (class, "CLIENT", "CLIENT"),
        (routine, "run (", "run"),
        (parameter, "target: TARGET", "target"),
    ] {
        let start = u64::try_from(original.find(literal).unwrap()).unwrap();
        let expected = SourceSpan::new(
            client.id,
            start,
            start + u64::try_from(spelling.len()).unwrap(),
        )
        .unwrap();
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let definition = definitions[0];
        assert_eq!(definition.source.span(), expected);
        assert_eq!(definition.repository, receipt.repository);
        assert_eq!(definition.generation, receipt.generation);
        assert_eq!(definition.file, client.id);
        assert_eq!(definition.source.repository(), receipt.repository);
        assert_eq!(definition.source.generation(), receipt.generation);
        assert_eq!(definition.source.content_hash(), physical_hash);
        assert_eq!(definition.provenance, module.provenance);
        assert_eq!(definition.evidence, entity.evidence);
        let declaration = definition.evidence.source.as_ref().unwrap();
        assert_eq!(declaration.repository(), receipt.repository);
        assert_eq!(declaration.generation(), receipt.generation);
        assert_eq!(declaration.span().file(), client.id);
        assert_eq!(declaration.content_hash(), physical_hash);
        assert!(declaration.span().start_byte() <= expected.start_byte());
        assert!(declaration.span().end_byte() >= expected.end_byte());
    }
    let start = u64::try_from(original.find("target.run").unwrap()).unwrap();
    let receiver = SourceSpan::new(client.id, start, start + 6).unwrap();
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Read)
        .collect();
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    assert_eq!(read.syntax_kind, "eiffel.formal_receiver.read");
    assert_eq!(read.source.span(), receiver);
    assert_eq!(read.repository, receipt.repository);
    assert_eq!(read.generation, receipt.generation);
    assert_eq!(read.file, client.id);
    assert_eq!(read.source.repository(), receipt.repository);
    assert_eq!(read.source.generation(), receipt.generation);
    assert_eq!(read.source.content_hash(), physical_hash);
    assert_eq!(read.syntactic_text_hash, content_hash(b"target"));
    assert_eq!(read.enclosing, Some(routine.id));
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
    assert_eq!(read.provenance, module.provenance);
    assert_eq!(read.evidence.source.as_ref(), Some(&read.source));
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference && site.source.span() == receiver)
        .collect();
    assert_eq!(references.len(), 1);
    assert!(matches!(
        references[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_ne!(read.id, references[0].id);
    assert_eq!(read.source, references[0].source);
    assert_eq!(read.provenance, references[0].provenance);
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| edge.predicate != RelationPredicate::Contains)
        .collect();
    assert_eq!(edges.len(), 1);
    let edge = edges[0];
    assert_eq!(edge.predicate, RelationPredicate::RefersTo);
    assert_eq!(edge.subject, RelationEndpoint::Occurrence(read.id));
    assert_eq!(edge.object, RelationEndpoint::Entity(parameter.id));
    assert_eq!(edge.repository, receipt.repository);
    assert_eq!(edge.generation, receipt.generation);
    assert_eq!(edge.provenance, read.provenance);
    assert_eq!(edge.evidence, read.evidence);
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate != RelationPredicate::Calls)
    );
    eprintln!("published_eiffel_formal_read={read:#?} exact_refers_to={edge:#?}");
    for gap in [
        "eiffel-feature-dispatch-target-unavailable",
        "eiffel-variable-or-argumentless-feature-binding-unavailable",
        "eiffel-type-and-inheritance-target-unavailable",
    ] {
        assert!(document.skipped_regions.iter().any(|row| row.detail == gap));
    }
    for (path, original) in inputs {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        let reference = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, u64::try_from(original.len()).unwrap()).unwrap(),
            content_hash(original.as_bytes()),
            None,
        );
        let source = service
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
        assert_eq!(source.data.chunks.len(), 1);
        assert_eq!(source.data.chunks[0].reference, reference);
        assert_eq!(source.data.chunks[0].bytes, original.as_bytes());
        assert!(document.occurrences.iter().any(|occurrence| {
            occurrence.file == file.id
                && occurrence.role == OccurrenceRole::Definition
                && matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
        }));
    }
    let no_op = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(no_op.generation, receipt.generation);
    service
        .inspect_generation(
            receipt.repository,
            Some(no_op.generation),
            &cancellation,
            |snapshot| {
                assert_eq!(snapshot.document(), &document);
            },
        )
        .unwrap();
}
