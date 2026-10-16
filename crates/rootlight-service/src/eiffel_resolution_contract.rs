//! Written Eiffel spelling must not become generic lexical binding evidence.

use super::*;
use rootlight_ir::OccurrenceTarget;
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
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
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
}
