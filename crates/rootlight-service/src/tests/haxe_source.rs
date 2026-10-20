//! Original Haxe declarations and a local argument keep authored source evidence.
//! Macro expansion remains outside this source-level public regression.

use super::*;
use rootlight_ir::RelationEndpoint;

#[test]
fn original_haxe_macro_host_local_read_and_literal_sources_are_exact() {
    let raw =
        include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/haxe/MacroTools.hx");
    assert_eq!(raw.len(), 1843);
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("written.hx"), raw).unwrap();
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
        .find(|file| file.path == "written.hx")
        .unwrap();
    assert_eq!(file.byte_length, 1843);
    assert_eq!(file.content_hash, content_hash(raw));
    let whole = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, 1843).unwrap(),
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
        ("MacroTools", &[(6_u64, 16_u64), (1589, 1599)][..]),
        (
            "__rootlight_absent_55ada34867ac9313363cb97f498a4d4de6b83fb927c55d02d00dc30cc20b235c__",
            &[][..],
        ),
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
        assert_eq!(found.matches.len(), expected.len());
        for (reference, (start, end)) in found.matches.iter().zip(expected) {
            assert_eq!(reference.span().start_byte(), *start);
            assert_eq!(reference.span().end_byte(), *end);
            assert_eq!(reference.repository(), receipt.repository);
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.content_hash(), file.content_hash);
            let read = service
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
            assert_eq!(read.data.chunks.len(), 1);
            assert_eq!(read.data.chunks[0].reference, *reference);
            assert_eq!(read.data.chunks[0].bytes, query.as_bytes());
        }
    }
    let definition = |name: &str, kind, start, end| {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == kind && entity.canonical_name == name && entity.language == "haxe"
            })
            .collect();
        assert_eq!(entities.len(), 1, "{kind:?}:{name}");
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().file() == file.id
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(sites.len(), 1, "{name}");
        assert_eq!(
            sites[0].target,
            OccurrenceTarget::Resolved {
                symbol: entities[0].id
            }
        );
        assert_eq!(sites[0].source.repository(), receipt.repository);
        assert_eq!(sites[0].source.generation(), receipt.generation);
        assert_eq!(sites[0].source.content_hash(), file.content_hash);
        entities[0].id
    };
    let class = definition("MacroTools", EntityKind::Class, 6, 16);
    let method = definition("buildLatestReleaseNotes", EntityKind::Method, 464, 487);
    let variable = definition("raw", EntityKind::Variable, 526, 529);
    for (owner, member, start, end) in [(class, method, 464, 487), (method, variable, 526, 529)] {
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(owner)
                    && edge.object == RelationEndpoint::Entity(member)
            })
            .collect();
        assert_eq!(edges.len(), 1);
        let source = edges[0].evidence.source.as_ref().unwrap();
        assert!(source.span().start_byte() <= start && source.span().end_byte() >= end);
        assert_eq!(source.content_hash(), file.content_hash);
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(variable)
                && edge.object == RelationEndpoint::Entity(method))
    );
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().file() == file.id
                && site.source.span().start_byte() == 609
                && site.source.span().end_byte() == 612
        })
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved { symbol: variable }
    );
    assert_eq!(reads[0].enclosing, Some(method));
    assert_eq!(reads[0].source.repository(), receipt.repository);
    assert_eq!(reads[0].source.generation(), receipt.generation);
    assert_eq!(reads[0].source.content_hash(), file.content_hash);
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 609
                && site.source.span().end_byte() == 612)
    );
    assert_eq!(file.language, "haxe");
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
fn haxe_local_reads_cannot_acquire_identically_named_members_of_another_source() {
    let directory = TempDir::new().unwrap();
    let first = b"class First { static function measure() { var value = 1; consume(value); } }";
    let second = b"class Second { static function measure() { consume(value); } }";
    fs::write(directory.path().join("first.hx"), first).unwrap();
    fs::write(directory.path().join("second.hx"), second).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let files: Vec<_> = ["first.hx", "second.hx"]
        .map(|path| {
            document
                .files
                .iter()
                .find(|file| file.path == path)
                .unwrap()
        })
        .into_iter()
        .collect();
    for (file, raw) in files.iter().zip([first.as_slice(), second.as_slice()]) {
        assert_eq!(file.language, "haxe");
        assert_eq!(file.content_hash, content_hash(raw));
        let reference = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, u64::try_from(raw.len()).unwrap()).unwrap(),
            file.content_hash,
            None,
        );
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
        assert_eq!(exact.data.chunks[0].bytes, raw);
    }
    let read = |file| {
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.file == file && site.role == OccurrenceRole::Read)
            .collect();
        assert_eq!(sites.len(), 1);
        sites[0]
    };
    let first_read = read(files[0].id);
    let second_read = read(files[1].id);
    let OccurrenceTarget::Resolved { symbol } = first_read.target else {
        panic!("first native local must resolve")
    };
    let target = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(target.kind, EntityKind::Variable);
    assert_eq!(
        target.evidence.source.as_ref().unwrap().span().file(),
        files[0].id
    );
    assert!(matches!(
        second_read.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(!document.occurrences.iter().any(
        |site| site.file == files[1].id && site.target == OccurrenceTarget::Resolved { symbol }
    ));
    assert!(document.skipped_regions.iter().any(|gap| gap.domain
        == rootlight_ir::FactDomain::Relations
        && gap.source.span().file() == files[1].id
        && gap.source.span() == second_read.source.span()
        && gap.source.content_hash() == second_read.source.content_hash()
        && gap.detail == "haxe-native-local-variable-binding-unavailable"));
}

fn read_haxe_physical(
    service: &FirstSliceService,
    generation: GenerationId,
    reference: SourceRef,
    expected: &[u8],
) {
    let read = service
        .source_read_with_options_and_budget(
            generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(rootlight_source::SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, reference);
    assert_eq!(read.data.chunks[0].bytes, expected);
}

#[test]
fn haxe_original_utf8_bom_and_utf16_keep_physical_source_definitions_relations_and_literals() {
    let original =
        include_str!("../../../rootlight-adapter-treesitter/tests/fixtures/haxe/MacroTools.hx");
    assert_eq!(original.len(), 1843);
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| -> Vec<u8> {
            if encoding == "utf-8" {
                text.as_bytes().to_vec()
            } else {
                text.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            }
        };
        let mut raw = if encoding == "utf-8" {
            vec![0xef, 0xbb, 0xbf]
        } else if encoding == "utf-16le" {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        let bom = raw.len();
        raw.extend(encode(original));
        let directory = TempDir::new().unwrap();
        fs::write(directory.path().join("written.hx"), &raw).unwrap();
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
            .find(|file| file.path == "written.hx")
            .unwrap();
        assert_eq!(file.language, "haxe");
        assert_eq!(file.encoding, encoding);
        assert_eq!(file.content_hash, content_hash(&raw));
        assert_eq!(file.byte_length, u64::try_from(raw.len()).unwrap());
        let physical = |start: usize, end: usize| {
            SourceSpan::new(
                file.id,
                u64::try_from(bom + encode(&original[..start]).len()).unwrap(),
                u64::try_from(bom + encode(&original[..end]).len()).unwrap(),
            )
            .unwrap()
        };
        read_haxe_physical(
            &service,
            receipt.generation,
            SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
                file.content_hash,
                None,
            ),
            &raw,
        );
        let definition = |name: &str, kind, start, end| {
            let span = physical(start, end);
            let sites: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition && site.source.span() == span
                })
                .collect();
            assert_eq!(sites.len(), 1, "{encoding}:{name}");
            let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
                panic!("exact encoded definition")
            };
            let entity = document
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap();
            assert_eq!(entity.kind, kind);
            assert_eq!(entity.canonical_name, name);
            assert_eq!(sites[0].source.content_hash(), file.content_hash);
            assert_eq!(sites[0].source.generation(), receipt.generation);
            read_haxe_physical(
                &service,
                receipt.generation,
                sites[0].source.clone(),
                &encode(name),
            );
            symbol
        };
        let class = definition("MacroTools", EntityKind::Class, 6, 16);
        let method = definition("buildLatestReleaseNotes", EntityKind::Method, 464, 487);
        let variable = definition("raw", EntityKind::Variable, 526, 529);
        for (owner, member, start, end) in [(class, method, 464, 487), (method, variable, 526, 529)]
        {
            let edges: Vec<_> = document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(owner)
                        && edge.object == RelationEndpoint::Entity(member)
                })
                .collect();
            assert_eq!(edges.len(), 1, "{encoding}");
            let reference = edges[0].evidence.source.as_ref().unwrap();
            let name = physical(start, end);
            assert!(
                reference.span().start_byte() <= name.start_byte()
                    && reference.span().end_byte() >= name.end_byte()
            );
            assert_eq!(reference.content_hash(), file.content_hash);
            let span = reference.span();
            read_haxe_physical(
                &service,
                receipt.generation,
                reference.clone(),
                &raw[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
            );
        }
        let read_span = physical(609, 612);
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Read && site.source.span() == read_span)
            .collect();
        assert_eq!(sites.len(), 1);
        assert_eq!(
            matches!(sites[0].target, OccurrenceTarget::Resolved { .. }),
            encoding == "utf-8"
        );
        if encoding != "utf-8" {
            assert!(document.skipped_regions.iter().any(|gap| gap.domain
                == rootlight_ir::FactDomain::Relations
                && gap.detail == "haxe-native-local-variable-binding-unavailable"
                && gap.source.span() == read_span
                && gap.source.content_hash() == file.content_hash));
        }
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.source.span() == read_span)
        );
        for (query, ranges) in [
            ("MacroTools", vec![physical(6, 16), physical(1589, 1599)]),
            ("__rootlight_absent_haxe_encoding__", Vec::new()),
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
            assert_eq!(found.matches.len(), ranges.len());
            for (reference, expected) in found.matches.iter().zip(ranges) {
                assert_eq!(reference.span(), expected);
                assert_eq!(reference.content_hash(), file.content_hash);
                assert_eq!(reference.generation(), receipt.generation);
                read_haxe_physical(
                    &service,
                    receipt.generation,
                    reference.clone(),
                    &encode(query),
                );
            }
        }
    }
}

#[test]
fn haxe_unsupported_selectors_splices_and_type_quotes_preserve_public_exact_source_and_reasons() {
    let cases: [(&str, &[u8], &str); 10] = [
        (
            "selector-a",
            include_bytes!(
                "../../../rootlight-adapter-treesitter/tests/fixtures/haxe/unsupported/selector-a.hx"
            ),
            "haxe-macro-expansion-and-runtime-effects-unavailable",
        ),
        (
            "selector-b",
            include_bytes!(
                "../../../rootlight-adapter-treesitter/tests/fixtures/haxe/unsupported/selector-b.hx"
            ),
            "haxe-macro-expansion-and-runtime-effects-unavailable",
        ),
        (
            "selector-e",
            include_bytes!(
                "../../../rootlight-adapter-treesitter/tests/fixtures/haxe/unsupported/selector-e.hx"
            ),
            "haxe-macro-expansion-and-runtime-effects-unavailable",
        ),
        (
            "selector-i",
            include_bytes!(
                "../../../rootlight-adapter-treesitter/tests/fixtures/haxe/unsupported/selector-i.hx"
            ),
            "haxe-macro-expansion-and-runtime-effects-unavailable",
        ),
        (
            "selector-p",
            include_bytes!(
                "../../../rootlight-adapter-treesitter/tests/fixtures/haxe/unsupported/selector-p.hx"
            ),
            "haxe-macro-expansion-and-runtime-effects-unavailable",
        ),
        (
            "selector-v",
            include_bytes!(
                "../../../rootlight-adapter-treesitter/tests/fixtures/haxe/unsupported/selector-v.hx"
            ),
            "haxe-macro-expansion-and-runtime-effects-unavailable",
        ),
        (
            "selector-empty",
            include_bytes!(
                "../../../rootlight-adapter-treesitter/tests/fixtures/haxe/unsupported/selector-empty.hx"
            ),
            "haxe-macro-expansion-and-runtime-effects-unavailable",
        ),
        (
            "nested-block-splice",
            include_bytes!(
                "../../../rootlight-adapter-treesitter/tests/fixtures/haxe/unsupported/nested-block-splice-queued.hx"
            ),
            "haxe-source-construct-semantics-unavailable",
        ),
        (
            "type-reification",
            include_bytes!(
                "../../../rootlight-adapter-treesitter/tests/fixtures/haxe/unsupported/type-reification-queued.hx"
            ),
            "haxe-macro-expansion-and-runtime-effects-unavailable",
        ),
        (
            "unknown-selector",
            include_bytes!(
                "../../../rootlight-adapter-treesitter/tests/fixtures/haxe/unsupported/unknown-reification-selector.hx"
            ),
            "haxe-source-construct-semantics-unavailable",
        ),
    ];
    for (name, raw, reason) in cases {
        let directory = TempDir::new().unwrap();
        fs::write(directory.path().join("written.hx"), raw).unwrap();
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
            .find(|file| file.path == "written.hx")
            .unwrap();
        assert_eq!(file.content_hash, content_hash(raw), "{name}");
        assert_eq!(file.byte_length, u64::try_from(raw.len()).unwrap());
        assert_eq!(file.language, "haxe");
        read_haxe_physical(
            &service,
            receipt.generation,
            SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
                file.content_hash,
                None,
            ),
            raw,
        );
        assert!(
            document.skipped_regions.iter().any(|gap| gap.domain
                == rootlight_ir::FactDomain::Relations
                && gap.detail == reason
                && gap.source.span().file() == file.id
                && gap.source.content_hash() == file.content_hash),
            "missing exact reason {reason}: {name}"
        );
        if name == "type-reification" {
            // The pinned native grammar recovers valid `macro : Type` syntax;
            // quotation gaps do not certify that recovered type as a host fact.
            assert!(document.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "syntax-error-recovery"
                    && diagnostic.coverage_effect == rootlight_ir::CoverageStatus::Unknown
                    && diagnostic.source.as_ref().is_some_and(|reference| {
                        reference.span().file() == file.id
                            && reference.content_hash() == file.content_hash
                            && reference.generation() == receipt.generation
                    })
            }));
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.domain == rootlight_ir::FactDomain::Diagnostics
                    && gap.reason == rootlight_ir::SkippedRegionReason::ParseError
                    && gap.detail == "syntax-error-recovery"
                    && gap.source.span().file() == file.id
                    && gap.source.content_hash() == file.content_hash
                    && gap.source.generation() == receipt.generation
            }));
            assert!(document.coverage_records.iter().any(|coverage| {
                coverage.scope == rootlight_ir::CoverageScope::File(file.id)
                    && coverage.domain == rootlight_ir::FactDomain::Diagnostics
                    && coverage.status == rootlight_ir::CoverageStatus::Unknown
            }));
        }
        assert!(!document.entities.iter().any(|entity| ["value","phantom","Int"].contains(&entity.canonical_name.as_str())), "fabricated declaration: {name}");
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "fabricated Read: {name}"
        );
        for query in ["macro", "__rootlight_absent_haxe_splice__"] {
            let expected: Vec<_> = raw
                .windows(query.len())
                .enumerate()
                .filter_map(|(start, bytes)| {
                    (bytes == query.as_bytes()).then_some((start, start + query.len()))
                })
                .collect();
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
            assert_eq!(found.matches.len(), expected.len(), "{name}:{query}");
            for (reference, (start, end)) in found.matches.iter().zip(expected) {
                assert_eq!(
                    reference.span(),
                    SourceSpan::new(
                        file.id,
                        u64::try_from(start).unwrap(),
                        u64::try_from(end).unwrap()
                    )
                    .unwrap()
                );
                assert_eq!(reference.repository(), receipt.repository);
                assert_eq!(reference.generation(), receipt.generation);
                assert_eq!(reference.content_hash(), file.content_hash);
                read_haxe_physical(
                    &service,
                    receipt.generation,
                    reference.clone(),
                    query.as_bytes(),
                );
            }
        }
    }
}
