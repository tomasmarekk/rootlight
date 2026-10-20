//! Keep original SCSS mixin formals, local include targets and physical reads.
//! The byte-for-byte fixture supplies the oracle; product output supplies no
//! expected names, kinds, source ranges or relation endpoints.

use super::*;
use rootlight_ir::{ContainerRef, RelationEndpoint};

#[test]
fn original_scss_mixin_formal_and_direct_calls_keep_exact_source() {
    let directory = TempDir::new().unwrap();
    let raw = include_bytes!("fixtures/scss/original-rounded-corners-mixin.scss");
    assert_eq!(raw.len(), 1994);
    fs::write(directory.path().join("original.scss"), raw).unwrap();
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
        .find(|file| file.path == "original.scss")
        .unwrap();
    assert_eq!(file.language, "scss");
    assert_eq!(file.byte_length, 1994);
    assert_eq!(file.content_hash, content_hash(raw));
    let definition = |name: &str, kind, start: u64, end: u64| {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "scss"
                    && entity.kind == kind
                    && entity.canonical_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .collect();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.file == file.id
                    && site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(
            sites.len(),
            1,
            "exact {kind:?} Definition {name}@{start}..{end}"
        );
        let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
            panic!("resolved authored Definition")
        };
        let entity = entities
            .into_iter()
            .find(|entity| entity.id == symbol)
            .expect("exact written entity");
        let physical = usize::try_from(start).unwrap()..usize::try_from(end).unwrap();
        assert_eq!(&raw[physical], name.as_bytes());
        assert_eq!(sites[0].source.repository(), receipt.repository);
        assert_eq!(sites[0].source.generation(), receipt.generation);
        assert_eq!(sites[0].source.content_hash(), file.content_hash);
        entity.id
    };
    let target = definition("border-image-repeat", EntityKind::Function, 945, 964);
    let rounded = definition("rounded-corners", EntityKind::Function, 1263, 1278);
    let compact = definition("compact-rounded-corners", EntityKind::Function, 1621, 1644);
    let parameter = definition("isDark", EntityKind::Parameter, 1280, 1286);
    assert_eq!(
        document
            .entities
            .iter()
            .find(|entity| entity.id == parameter)
            .unwrap()
            .container,
        Some(ContainerRef::Entity(rounded))
    );
    for (predicate, subject, object, start, end) in [
        (RelationPredicate::Contains, rounded, parameter, 1280, 1286),
        (RelationPredicate::Calls, rounded, target, 1402, 1421),
        (RelationPredicate::Calls, compact, target, 1768, 1787),
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
        assert_eq!(edges.len(), 1, "original {predicate:?} at {start}..{end}");
        let source = edges[0].evidence.source.as_ref().unwrap();
        assert_eq!(source.repository(), receipt.repository);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.content_hash(), file.content_hash);
        assert_eq!(source.span().file(), file.id);
        assert!(source.span().start_byte() <= start && source.span().end_byte() >= end);
    }
    for (owner, start, end) in [(rounded, 1402, 1421), (compact, 1768, 1787)] {
        let calls: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.file == file.id
                    && site.role == OccurrenceRole::CallSite
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].target,
            OccurrenceTarget::Resolved { symbol: target }
        );
        assert_eq!(calls[0].enclosing, Some(owner));
    }
    assert!(!document.occurrences.iter().any(|site| site.file == file.id
        && site.role == OccurrenceRole::Definition
        && site.source.span().start_byte() == 1402
        && site.source.span().end_byte() == 1421));
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
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, whole);
    assert_eq!(read.data.chunks[0].bytes, raw);
    for (needle, ranges) in [
        (
            "border-image-repeat",
            vec![
                (945, 964),
                (972, 991),
                (1114, 1133),
                (1215, 1234),
                (1402, 1421),
                (1768, 1787),
            ],
        ),
        ("border-image-repeat__absent_source_control__", Vec::new()),
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
            assert_eq!(source.repository(), receipt.repository);
            assert_eq!(source.generation(), receipt.generation);
            assert_eq!(source.content_hash(), file.content_hash);
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![source.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(SourceEncoding::Bytes),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(read.data.chunks[0].reference, source);
            assert_eq!(read.data.chunks[0].bytes, needle.as_bytes());
        }
    }
}

#[test]
fn includes_cannot_borrow_another_file_target_and_keep_exact_source_access() {
    let directory = TempDir::new().unwrap();
    let target = b"@mixin target() { color: red; }\n";
    let caller = b"@mixin caller($flag: false) { @include target(); }\n";
    fs::write(directory.path().join("target.scss"), target).unwrap();
    fs::write(directory.path().join("caller.scss"), caller).unwrap();
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
        .find(|file| file.path == "caller.scss")
        .unwrap();
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.file == file.id
                && site.role == OccurrenceRole::CallSite
                && site.syntax_kind == "scss.mixin.call"
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.source == sites[0].source
                && gap.detail == "scss-mixin-target-unavailable")
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
    let whole = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, caller.len() as u64).unwrap(),
        content_hash(caller),
        None,
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![whole.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].reference, whole);
    assert_eq!(read.data.chunks[0].bytes, caller);
}
