//! Preserve the authored Hare declarations, field owner and direct call witness.

use super::*;
use rootlight_ir::{ContainerRef, RelationEndpoint};

#[test]
fn original_hare_struct_field_function_call_and_source_are_exact() {
    let directory = TempDir::new().unwrap();
    let raw = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/hare/original-part1.ha"
    );
    assert_eq!(raw.len(), 1742);
    fs::write(directory.path().join("original.ha"), raw).unwrap();
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
        .find(|file| file.path == "original.ha")
        .unwrap();
    assert_eq!(file.content_hash, content_hash(raw));
    assert_eq!(file.byte_length, 1742);
    assert_eq!(file.language, "hare");
    let definition = |name: &str, kind, start, end| {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "hare"
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
    let structure = definition("equation", EntityKind::Struct, 63, 71);
    let field = definition("test", EntityKind::Field, 85, 89);
    let target = definition("read_input", EntityKind::Function, 121, 131);
    let caller = definition("main", EntityKind::Function, 1408, 1412);
    assert_eq!(
        document
            .entities
            .iter()
            .find(|entity| entity.id == field)
            .unwrap()
            .container,
        Some(ContainerRef::Entity(structure))
    );
    for (predicate, subject, object, start, end) in [
        (RelationPredicate::Contains, structure, field, 85, 89),
        (RelationPredicate::Calls, caller, target, 1444, 1454),
    ] {
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == predicate
                    && edge.subject == RelationEndpoint::Entity(subject)
                    && edge.object == RelationEndpoint::Entity(object)
            })
            .collect();
        assert_eq!(edges.len(), 1, "original {predicate:?}");
        let source = edges[0].evidence.source.as_ref().unwrap();
        assert_eq!(source.repository(), receipt.repository);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.span().file(), file.id);
        assert_eq!(source.content_hash(), file.content_hash);
        assert!(source.span().start_byte() <= start && source.span().end_byte() >= end);
    }
    let calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.file == file.id
                && site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == 1444
                && site.source.span().end_byte() == 1454
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    assert_eq!(calls[0].enclosing, Some(caller));
    assert!(!document.occurrences.iter().any(|site| site.file == file.id
        && site.role == OccurrenceRole::Definition
        && site.source.span().start_byte() == 1444
        && site.source.span().end_byte() == 1454));
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
        ("read_input", vec![(121, 131), (1444, 1454)]),
        (
            "__absent_hare_0e8327c63b8a4fc1b688a028ffd136ab__",
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
            .any(|gap| gap.detail.as_str().starts_with("hare-"))
    );
}

#[test]
fn hare_shadow_import_duplicate_and_recovery_barriers_keep_exact_source() {
    for text in [
        "fn target() void = {}; fn caller(target: fn() void) void = { target(); };",
        "fn target() void = {}; fn caller() void = { let target = 1; target(); };",
        "fn target() void = {}; fn caller() void = { for (const target .. values) { target(); }; };",
        "use other::{target}; fn target() void = {}; fn caller() void = { target(); };",
        "use other::*; fn target() void = {}; fn caller() void = { target(); };",
        "fn target() void = {}; fn target() void = {}; fn caller() void = { target(); };",
        "fn target() void; fn caller() void = { target(); };",
        "@init fn target() void = {}; fn caller() void = { target(); };",
        "fn target() void = {}; fn caller() void = { other::target(); };",
        "fn target() void = {}; fn caller() void = { object.target(); };",
        "fn target() void = {}; fn caller() void = { target(; };",
        "fn caller() void = { target(); }; fn target() void = {};",
    ] {
        let directory = TempDir::new().unwrap();
        fs::write(directory.path().join("barrier.ha"), text).unwrap();
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
            .find(|file| file.path == "barrier.ha")
            .unwrap();
        assert_eq!(file.language, "hare");
        assert_eq!(file.content_hash, content_hash(text.as_bytes()));
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls),
            "{text}"
        );
        assert!(
            !document.occurrences.iter().any(|site| site.file == file.id
                && site.role == OccurrenceRole::CallSite
                && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{text}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().file() == file.id
                    && gap.detail.as_str().starts_with("hare-")),
            "{text}"
        );
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
        assert_eq!(read.data.chunks[0].bytes, text.as_bytes());
    }
}

#[test]
fn hare_other_files_do_not_supply_spelling_only_call_targets() {
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("target.ha"), "fn target() void = {};").unwrap();
    fs::write(
        directory.path().join("caller.ha"),
        "fn caller() void = { target(); };",
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
        document
            .entities
            .iter()
            .any(|entity| entity.language == "hare"
                && entity.kind == EntityKind::Function
                && entity.canonical_name == "target")
    );
    let caller = document
        .files
        .iter()
        .find(|file| file.path == "caller.ha")
        .unwrap();
    let calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.file == caller.id && site.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    assert!(matches!(
        calls[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn hare_original_bom_and_both_utf16_orders_keep_physical_calls_and_reads() {
    let original = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/hare/original-part1.ha"
    );
    let text = std::str::from_utf8(original).unwrap();
    assert!(original.is_ascii());
    for encoding in 0..3 {
        let raw: Vec<u8> = match encoding {
            0 => [b"\xef\xbb\xbf".as_slice(), original.as_slice()].concat(),
            1 => [
                vec![0xff, 0xfe],
                text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            ]
            .concat(),
            _ => [
                vec![0xfe, 0xff],
                text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            ]
            .concat(),
        };
        let physical = |offset: u64| {
            if encoding == 0 {
                offset + 3
            } else {
                offset * 2 + 2
            }
        };
        let directory = TempDir::new().unwrap();
        fs::write(directory.path().join("encoded.ha"), &raw).unwrap();
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
            .find(|file| file.path == "encoded.ha")
            .unwrap();
        assert_eq!(file.content_hash, content_hash(&raw));
        for (name, kind, start, end) in [
            ("equation", EntityKind::Struct, 63, 71),
            ("test", EntityKind::Field, 85, 89),
            ("read_input", EntityKind::Function, 121, 131),
            ("main", EntityKind::Function, 1408, 1412),
        ] {
            let entities: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.language == "hare"
                        && entity.kind == kind
                        && entity.canonical_name == name
                })
                .collect();
            assert_eq!(entities.len(), 1, "{encoding}:{name}");
            let sites: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.file == file.id
                        && site.role == OccurrenceRole::Definition
                        && site.target
                            == (OccurrenceTarget::Resolved {
                                symbol: entities[0].id,
                            })
                })
                .collect();
            assert_eq!(sites.len(), 1);
            assert_eq!(
                (
                    sites[0].source.span().start_byte(),
                    sites[0].source.span().end_byte()
                ),
                (physical(start), physical(end))
            );
        }
        let calls: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.file == file.id
                    && site.role == OccurrenceRole::CallSite
                    && site.source.span().start_byte() == physical(1444)
            })
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].source.span().end_byte(), physical(1454));
        assert!(matches!(calls[0].target, OccurrenceTarget::Resolved { .. }));
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
                    .with_context_lines_after(0)
                    .with_encoding(rootlight_source::SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].reference, whole);
        assert_eq!(read.data.chunks[0].bytes, raw);
    }
}

#[test]
fn hare_body_edit_reuses_written_symbol_but_advances_source_authority() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("body.ha");
    let mut service = FirstSliceService::new(2).unwrap();
    let mut previous = None;
    for text in [
        "fn target() int = { return 1; };",
        "fn target() int = { return 123456; };",
    ] {
        fs::write(&path, text).unwrap();
        let receipt = service
            .index_repository(directory.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let entity = snapshot
            .document()
            .entities
            .iter()
            .find(|entity| {
                entity.language == "hare"
                    && entity.kind == EntityKind::Function
                    && entity.canonical_name == "target"
            })
            .unwrap();
        if let Some((old_id, old_hash, old_generation)) = previous {
            assert_eq!(entity.id, old_id);
            assert_ne!(
                entity.evidence.source.as_ref().unwrap().content_hash(),
                old_hash
            );
            assert_ne!(receipt.generation, old_generation);
        }
        previous = Some((
            entity.id,
            entity.evidence.source.as_ref().unwrap().content_hash(),
            receipt.generation,
        ));
    }
}
