//! Derived VBA sources participate in the actual durable indexing lifecycle.
//! These fixtures are authored, not copied from a diagnostic repository.

use super::*;
use rootlight_ir::{ContainerRef, OccurrenceTarget};
use std::fs;

#[path = "fixture.rs"]
mod fixture;

fn tempdir() -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    {
        tempfile::tempdir_in("/private/tmp").unwrap()
    }
    #[cfg(not(target_os = "macos"))]
    {
        tempfile::tempdir().unwrap()
    }
}

const FIRST: &[u8] = b"Attribute VB_Name = \"Alpha\"\r\nPublic Function Compute() As Long\r\n Compute = 1\r\nEnd Function\r\n";
const SECOND: &[u8] = b"Attribute VB_Name = \"Beta\"\r\nPublic Sub Dispatch()\r\nEnd Sub\r\n";

fn reference_book(major: u16) -> (Vec<u8>, Vec<Vec<u8>>) {
    reference_book_with_names(major, None, None)
}

fn reference_book_with_names(
    major: u16,
    leading: Option<(&[u8], &str)>,
    extended: Option<(&[u8], &str)>,
) -> (Vec<u8>, Vec<Vec<u8>>) {
    fn text(out: &mut Vec<u8>, value: &[u8]) {
        out.extend(u32::try_from(value.len()).unwrap().to_le_bytes());
        out.extend(value);
    }
    fn marker(out: &mut Vec<u8>, id: u16) {
        out.extend(id.to_le_bytes());
        out.extend(0u32.to_le_bytes());
    }
    fn name(out: &mut Vec<u8>, pair: Option<(&[u8], &str)>) {
        if let Some((mbcs, unicode)) = pair {
            out.extend(0x0016u16.to_le_bytes());
            text(out, mbcs);
            out.extend(0x003eu16.to_le_bytes());
            text(
                out,
                &unicode
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
        }
    }
    let first = format!("*\\G{{01234567-89AB-CDEF-1032-547698BADCFE}}#{major:x}.2#409#C:\\unavailable\\first.tlb#LibraryShared").into_bytes();
    let second = b"*\\G{01234567-89AB-CDEF-1032-547698BADCFE}#1.3#409#C:\\unavailable\\second.tlb#LibraryShared".to_vec();
    let mut records = Vec::new();
    for identifier in [&first[..], &second[..], b"malformed-library".as_slice()] {
        name(&mut records, leading);
        marker(&mut records, 0x000d);
        text(&mut records, identifier);
        records.extend([0; 6]);
    }
    let project_start = records.len();
    name(&mut records, leading);
    marker(&mut records, 0x000e);
    text(&mut records, b"*\\CC:\\unavailable\\project.xlsm");
    text(&mut records, b"*\\C../project.xlsm");
    records.extend(1u32.to_le_bytes());
    records.extend(2u16.to_le_bytes());
    let project_record = records[project_start..].to_vec();
    name(&mut records, leading);
    records.extend(0x0033u16.to_le_bytes());
    text(&mut records, &first);
    marker(&mut records, 0x002f);
    text(
        &mut records,
        b"*\\G{00000000-0000-0000-0000-000000000000}#0.0#0##",
    );
    records.extend([0; 6]);
    name(&mut records, extended);
    records.extend([0; 6]);
    text(&mut records, &second);
    records.extend([0; 26]);
    let book = fixture::native_book_with_references(
        &[(
            "Alpha",
            FIRST,
            rootlight_source::vba_project::ProjectModuleKind::Procedural,
        )],
        "\"Project\"",
        &records,
    );
    (
        book,
        vec![first.clone(), second.clone(), project_record, first, second],
    )
}

#[test]
fn declared_vba_reference_names_are_searchable_and_source_bound_after_restart() {
    let root = tempdir();
    let storage = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let (book, _) = reference_book_with_names(
        1,
        Some((b"Caf\xe9", "Café")),
        Some((b"ExtendedApi", "ExtendedApi")),
    );
    fs::write(root.path().join("book.xlsm"), &book).unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let mut first_sources = Vec::new();
    for restart in [false, true] {
        if restart {
            drop(service);
            service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        }
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let entities: Vec<_> = snapshot
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::ExternalSymbol)
            .collect();
        assert_eq!(entities.len(), 5);
        assert_eq!(
            entities
                .iter()
                .map(|entity| entity.id)
                .collect::<BTreeSet<_>>()
                .len(),
            5
        );
        let mut sources = Vec::new();
        for (query, count) in [("Café", 4), ("ExtendedApi", 1), ("LibraryShared", 4)] {
            let located = service
                .code_locate_with_languages_and_budget(
                    receipt.generation,
                    query.to_owned(),
                    LocateMode::Exact,
                    vec!["vba".to_owned()],
                    16,
                    0,
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            let symbols: BTreeSet<_> = located
                .data
                .hits
                .iter()
                .filter_map(|hit| hit.symbol)
                .collect();
            assert_eq!(symbols.len(), count, "{query}");
            assert_eq!(
                symbols,
                entities
                    .iter()
                    .filter(|entity| entity.qualified_name == query || entity.display_name == query)
                    .map(|entity| entity.id)
                    .collect()
            );
        }
        for entity in &entities {
            let source = entity.evidence.source.clone().unwrap();
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
            let bytes = &read.data.chunks[0].bytes;
            let mbcs = if entity.qualified_name == "Café" {
                b"Caf\xe9".as_slice()
            } else {
                b"ExtendedApi".as_slice()
            };
            let unicode: Vec<_> = entity
                .qualified_name
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            assert!(
                bytes.starts_with(mbcs) || bytes.windows(mbcs.len()).any(|window| window == mbcs)
            );
            assert!(bytes.windows(unicode.len()).any(|window| window == unicode));
            assert_eq!(source.generation(), receipt.generation);
            assert_eq!(
                read.data.chunks[0]
                    .extracted_from
                    .as_ref()
                    .unwrap()
                    .content_hash(),
                content_hash(&book)
            );
            let explained = service
                .symbol_explain(receipt.generation, entity.id, &deadline())
                .unwrap();
            assert_eq!(explained.data.entity, **entity);
            assert!(explained.data.occurrences.is_empty());
            sources.push(source);
        }
        if restart {
            assert_eq!(sources, first_sources);
        } else {
            first_sources = sources;
        }
    }
    let (changed, _) = reference_book_with_names(1, Some((b"RenamedApi", "RenamedApi")), None);
    fs::write(root.path().join("book.xlsm"), changed).unwrap();
    let updated = service.index_repository(root.path(), &deadline()).unwrap();
    assert_ne!(receipt.generation, updated.generation);
    for (query, count) in [("Café", 0), ("ExtendedApi", 0), ("RenamedApi", 4)] {
        let located = service
            .code_locate_with_languages_and_budget(
                updated.generation,
                query.to_owned(),
                LocateMode::Exact,
                vec!["vba".to_owned()],
                16,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            located
                .data
                .hits
                .iter()
                .filter_map(|hit| hit.symbol)
                .count(),
            count,
            "{query}"
        );
    }
    let prepared = service
        .prepare_repository_with_options(
            root.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
            &deadline(),
        )
        .unwrap();
    let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
    assert!(updated.logical_snapshot.is_some());
    assert_eq!(rebuilt.logical_snapshot, updated.logical_snapshot);
}

#[test]
fn inconsistent_vba_reference_names_do_not_hide_valid_library_declarations() {
    for (mbcs, unicode) in [
        (b"Alias".as_slice(), "ALIAS"),
        (b"".as_slice(), ""),
        (b"Caf\xe9".as_slice(), "Cafe\u{301}"),
        (b"\x81".as_slice(), "Alias"),
    ] {
        let (book, _) = reference_book_with_names(
            1,
            Some((mbcs, unicode)),
            Some((b"ExtendedApi", "ExtendedApi")),
        );
        let (prepared, _) = prepared_fixture(&book, 100, 128 * 1024 * 1024, &deadline());
        assert_eq!(prepared.references.len(), 5);
        let name_issues: Vec<_> = prepared
            .issues
            .iter()
            .filter(|(_, message)| message.ends_with("name rejected"))
            .collect();
        assert_eq!(name_issues.len(), 4, "{unicode}");
        assert!(
            name_issues
                .iter()
                .all(|(_, message)| public_diagnostic_message(message))
        );
    }
}

#[test]
fn conflicting_reference_name_is_not_an_indexed_alias() {
    let root = tempdir();
    let (book, _) = reference_book_with_names(
        1,
        Some((b"FalseAlias", "OtherAlias")),
        Some((b"ExtendedApi", "ExtendedApi")),
    );
    fs::write(root.path().join("book.xlsm"), book).unwrap();
    let mut service = FirstSliceService::new(4).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(
        receipt
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message.ends_with("name rejected"))
            .count(),
        4
    );
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let references: Vec<_> = snapshot
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::ExternalSymbol)
        .collect();
    assert_eq!(references.len(), 5);
    assert_eq!(
        references
            .iter()
            .filter(|entity| entity.qualified_name == entity.canonical_name)
            .count(),
        4
    );
    for (query, count) in [
        ("FalseAlias", 0),
        ("OtherAlias", 0),
        ("ExtendedApi", 1),
        ("LibraryShared", 4),
    ] {
        let located = service
            .code_locate_with_languages_and_budget(
                receipt.generation,
                query.to_owned(),
                LocateMode::Exact,
                vec!["vba".to_owned()],
                16,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            located
                .data
                .hits
                .iter()
                .filter_map(|hit| hit.symbol)
                .count(),
            count,
            "{query}"
        );
    }
}

#[test]
fn declared_vba_references_survive_restart_with_exact_binary_evidence_and_imports() {
    let root = tempdir();
    let storage = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let (book, expected) = reference_book(1);
    fs::write(root.path().join("book.xlsm"), &book).unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let mut first_sources = Vec::new();
    for restart in [false, true] {
        if restart {
            drop(service);
            service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        }
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let metadata = document
            .files
            .iter()
            .find(|file| file.path == "book.xlsm/@vba/references.bin")
            .unwrap();
        assert_eq!(metadata.encoding, "binary");
        assert_eq!(metadata.language, "vba");
        let mut entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.language == "vba" && entity.kind == EntityKind::ExternalSymbol)
            .collect();
        entities.sort_by_key(|entity| entity.evidence.source.as_ref().unwrap().span().start_byte());
        assert_eq!(entities.len(), expected.len());
        assert_ne!(entities[0].canonical_name, entities[1].canonical_name);
        assert_eq!(entities[0].display_name, entities[1].display_name);
        let mut sources = Vec::new();
        for (entity, expected) in entities.iter().zip(&expected) {
            assert_eq!(entity.kind, EntityKind::ExternalSymbol);
            assert!(entity.flags.contains(&rootlight_ir::EntityFlag::External));
            assert!(entity.flags.contains(&rootlight_ir::EntityFlag::Synthetic));
            let source = entity.evidence.source.clone().unwrap();
            assert_eq!(source.span().file(), metadata.id);
            assert_eq!(source.generation(), receipt.generation);
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
            assert_eq!(&read.data.chunks[0].bytes, expected);
            assert_eq!(
                read.data.chunks[0]
                    .extracted_from
                    .as_ref()
                    .unwrap()
                    .content_hash(),
                content_hash(&book)
            );
            let relationships = service
                .symbol_relationships(
                    receipt.generation,
                    BTreeSet::from([entity.id]),
                    vec![RelationFamily::Imports],
                    Some(RelationDirection::Inbound),
                    0,
                    16,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(relationships.data.returned_edges, 1);
            assert!(!relationships.data.truncated);
            assert!(
                relationships
                    .data
                    .groups
                    .iter()
                    .flat_map(|g| &g.items)
                    .any(|item| item.source_refs.contains(&source))
            );
            let explanation = service
                .symbol_explain(receipt.generation, entity.id, &deadline())
                .unwrap();
            assert_eq!(explanation.data.entity, **entity);
            assert!(explanation.data.occurrences.is_empty());
            sources.push(source);
        }
        assert!(
            document
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("vba reference 2 registered"))
        );
        assert!(receipt.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .starts_with("vba reference 2 registered invalid identifier at byte ")
        }));
        let located = service
            .code_locate_with_languages_and_budget(
                receipt.generation,
                "LibraryShared".to_owned(),
                LocateMode::Exact,
                vec!["vba".to_owned()],
                16,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            located
                .data
                .hits
                .iter()
                .filter_map(|hit| hit.symbol)
                .collect::<BTreeSet<_>>(),
            entities
                .iter()
                .filter(|entity| entity.display_name == "LibraryShared")
                .map(|entity| entity.id)
                .collect()
        );
        assert!(
            !document
                .occurrences
                .iter()
                .any(|occurrence| occurrence.file == metadata.id
                    && occurrence.role == OccurrenceRole::Definition)
        );
        for domain in [
            IrFactDomain::Entities,
            IrFactDomain::Relations,
            IrFactDomain::Extensions,
            IrFactDomain::Provenance,
        ] {
            let coverage = document
                .coverage_records
                .iter()
                .find(|record| {
                    record.scope == CoverageScope::File(metadata.id) && record.domain == domain
                })
                .unwrap();
            let extra = match domain {
                IrFactDomain::Entities => 1,
                IrFactDomain::Relations => 0,
                IrFactDomain::Extensions | IrFactDomain::Provenance => 2,
                _ => unreachable!("test only enumerates reference projection domains"),
            };
            assert_eq!(
                coverage.indexed,
                u64::try_from(expected.len()).unwrap() + extra
            );
            if matches!(domain, IrFactDomain::Entities | IrFactDomain::Relations) {
                assert_eq!(coverage.status, CoverageStatus::Unknown);
            }
            assert_eq!(coverage.discovered, coverage.indexed + coverage.skipped);
        }
        if restart {
            assert_eq!(sources, first_sources);
        } else {
            first_sources = sources;
        }
    }
    let (changed, changed_expected) = reference_book(2);
    fs::write(root.path().join("book.xlsm"), changed).unwrap();
    let updated = service.index_repository(root.path(), &deadline()).unwrap();
    assert_ne!(receipt.generation, updated.generation);
    let snapshot = service
        .loaded_generation_snapshot(updated.generation)
        .unwrap();
    let mut entities: Vec<_> = snapshot
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "vba" && entity.kind == EntityKind::ExternalSymbol)
        .collect();
    entities.sort_by_key(|entity| entity.evidence.source.as_ref().unwrap().span().start_byte());
    let read = service
        .source_read_with_options_and_budget(
            updated.generation,
            vec![entities[0].evidence.source.clone().unwrap()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, changed_expected[0]);
    let prepared = service
        .prepare_repository_with_options(
            root.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
            &deadline(),
        )
        .unwrap();
    let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
    assert!(updated.logical_snapshot.is_some());
    assert_eq!(rebuilt.logical_snapshot, updated.logical_snapshot);
    assert_eq!(rebuilt.entities, updated.entities);
    assert_eq!(rebuilt.indexed_files, updated.indexed_files);
}

#[test]
fn reference_admission_does_not_displace_source_modules() {
    let (book, _) = reference_book(1);
    let (prepared, snapshots) = prepared_fixture(&book, 1, 1_000_000, &deadline());
    assert_eq!(prepared.inputs.len(), 1);
    assert_eq!(snapshots.len(), 2);
    assert!(prepared.references.is_empty());
    assert!(prepared.inputs[0].path.ends_with(".bas"));
    assert!(
        prepared
            .issues
            .iter()
            .any(|(_, issue)| issue.contains("reference metadata admission exhausted"))
    );
}

#[test]
fn native_raised_events_keep_exact_relationships_through_rename_and_restart() {
    for kind in [
        ProjectModuleKind::Class,
        ProjectModuleKind::Document {
            automation_version: 1,
        },
        ProjectModuleKind::Designer,
    ] {
        let root = tempdir();
        let storage = tempdir();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let mut previous = None;
        for (event_name, resolved) in [("Changed", true), ("Renamed", false), ("Changed", true)] {
            let source = format!(
                "Attribute VB_Name = \"Emitter\"\r\nPublic Event {event_name}(ByVal value As Long)\r\nPublic Property Get Changed() As Long\r\nChanged = 1\r\nEnd Property\r\nPublic Sub Send(ByVal changed As Long)\r\nRaiseEvent CHANGED(changed)\r\nDim observed As Long\r\nobserved = Me.Changed\r\nEnd Sub\r\n"
            );
            let book = fixture::native_book(&[("Emitter", source.as_bytes(), kind)], "\"Project\"");
            fs::write(root.path().join("book.xlsm"), &book).unwrap();
            let mut service =
                FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
            let receipt = service.index_repository(root.path(), &deadline()).unwrap();
            assert_eq!(
                service.index_repository(root.path(), &deadline()).unwrap(),
                receipt
            );
            if let Some(previous) = previous {
                assert_ne!(receipt.generation, previous);
            }
            previous = Some(receipt.generation);
            drop(service);
            let mut service =
                FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            let event = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Event && entity.canonical_name == event_name
                })
                .unwrap();
            let property = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "Changed" && entity.kind != EntityKind::Event
                })
                .unwrap();
            let property_start =
                u64::try_from(source.rfind("Changed\r\nEnd Sub").unwrap()).unwrap();
            let property_site = document
                .occurrences
                .iter()
                .find(|site| site.source.span().start_byte() == property_start)
                .unwrap();
            assert_eq!(
                property_site.target,
                OccurrenceTarget::Resolved {
                    symbol: property.id
                }
            );
            assert!(
                document
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Calls
                        && edge.object == RelationEndpoint::Entity(property.id)
                        && edge.evidence.source.as_ref() == Some(&property_site.source))
            );
            let site = document
                .occurrences
                .iter()
                .find(|site| site.syntax_kind == "vba.event_identifier.reference")
                .unwrap();
            if resolved {
                assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: event.id });
            } else {
                assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            }
            for (detail, expected) in [
                ("vba-event-target-unavailable", !resolved),
                ("vba-event-handlers-unavailable", resolved),
            ] {
                assert_eq!(
                    document
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.source == site.source && gap.detail == detail),
                    expected
                );
            }
            let relationships = service
                .symbol_relationships(
                    receipt.generation,
                    BTreeSet::from([event.id]),
                    vec![RelationFamily::References],
                    Some(RelationDirection::Inbound),
                    0,
                    16,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(
                relationships
                    .data
                    .groups
                    .iter()
                    .flat_map(|group| &group.items)
                    .flat_map(|edge| &edge.source_refs)
                    .any(|source| source == &site.source),
                resolved
            );
            assert!(!relationships.data.truncated);
            assert!(!document.relations.iter().any(|edge| matches!(
                edge.predicate,
                RelationPredicate::Calls | RelationPredicate::DispatchCandidate
            ) && edge.evidence.source.as_ref()
                == Some(&site.source)));
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![site.source.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(read.data.chunks[0].bytes, b"CHANGED");
            assert_eq!(site.source.generation(), receipt.generation);
            assert_eq!(
                read.data.chunks[0]
                    .extracted_from
                    .as_ref()
                    .unwrap()
                    .content_hash(),
                content_hash(&book)
            );
            let prepared = service
                .prepare_repository_with_options(
                    root.path(),
                    FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                    &deadline(),
                )
                .unwrap();
            let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
            assert!(receipt.logical_snapshot.is_some());
            assert_eq!(rebuilt.logical_snapshot, receipt.logical_snapshot);
        }
    }
}

#[test]
fn native_statement_labels_survive_rename_noop_restart_and_clean_rebuild() {
    let root = tempdir();
    let storage = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let mut previous = None;
    for (name, resolved, conditional) in [
        ("Finish", true, false),
        ("Finish", true, true),
        ("Renamed", false, true),
        ("Finish", true, true),
        ("Finish", true, false),
    ] {
        let (prefix, suffix) = if conditional {
            ("#If Enabled Then\r\n", "#End If\r\n")
        } else {
            ("", "")
        };
        let source = format!(
            "Attribute VB_Name = \"Worker\"\r\nPublic Sub Run()\r\n{prefix}GoTo Finish\r\n{name}:\r\n{suffix}End Sub\r\n"
        );
        let book = fixture::typed_book(&[
            ("Worker", source.as_bytes(), false),
            ("Beta", SECOND, false),
        ]);
        fs::write(root.path().join("book.xlsm"), &book).unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        assert_ne!(previous, Some(receipt.generation));
        previous = Some(receipt.generation);
        drop(service);
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        assert_eq!(document.version, NormalizedIrVersion::V1_7);
        let label = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::StatementLabel && entity.canonical_name == name
            })
            .unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.statement_label.reference")
            .unwrap();
        assert_eq!(
            site.target == OccurrenceTarget::Resolved { symbol: label.id },
            resolved
        );
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == site.source
                    && gap.detail == "vba-statement-label-target-unavailable"),
            !resolved
        );
        let relationships = service
            .symbol_relationships(
                receipt.generation,
                BTreeSet::from([label.id]),
                vec![RelationFamily::References],
                Some(RelationDirection::Inbound),
                0,
                16,
                0,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            relationships
                .data
                .groups
                .iter()
                .flat_map(|group| &group.items)
                .flat_map(|edge| &edge.source_refs)
                .any(|reference| reference == &site.source),
            resolved
        );
        assert!(!relationships.data.truncated);
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![site.source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, b"Finish");
        assert_eq!(site.source.generation(), receipt.generation);
        assert_eq!(
            read.data.chunks[0]
                .extracted_from
                .as_ref()
                .unwrap()
                .content_hash(),
            content_hash(&book)
        );
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
        assert!(receipt.logical_snapshot.is_some());
        assert_eq!(rebuilt.logical_snapshot, receipt.logical_snapshot);
    }
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn read_module(
    service: &FirstSliceService,
    generation: GenerationId,
    path: &str,
    bytes: &[u8],
) -> SourceRef {
    let cancellation = deadline();
    let located = service
        .code_locate(
            generation,
            path.to_owned(),
            LocateMode::Exact,
            10,
            0,
            &cancellation,
        )
        .unwrap();
    let hit = located
        .data
        .hits
        .iter()
        .find(|hit| hit.symbol.is_none() && hit.path == path)
        .unwrap();
    let reference = hit.source.clone().unwrap();
    let result = service
        .source_read_with_options_and_budget(
            generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(result.data.chunks[0].bytes, bytes);
    assert_eq!(reference.content_hash(), content_hash(bytes));
    let origin = hit.extracted_from.as_ref().expect("derived locate origin");
    assert_eq!(result.data.chunks[0].extracted_from.as_ref(), Some(origin));
    assert_eq!(origin.repository(), reference.repository());
    assert_eq!(origin.generation(), generation);
    assert_ne!(origin.span().file(), reference.span().file());
    assert_eq!(origin.span().start_byte(), 0);
    assert!(origin.line_hint().is_none());
    let container = service
        .source_read_with_options_and_budget(
            generation,
            vec![origin.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(rootlight_source::SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    let container = &container.data.chunks[0];
    assert!(container.extracted_from.is_none());
    assert_eq!(content_hash(&container.bytes), origin.content_hash());
    assert_eq!(
        u64::try_from(container.bytes.len()).unwrap(),
        origin.span().end_byte()
    );
    reference
}

#[test]
fn automatic_object_receivers_keep_project_types_and_calls_after_restart() {
    for native in [false, true] {
        for declaration in [
            "Public Sub Run()\r\nDim receiver As New Worker\r\n",
            "Public Sub Run()\r\nDim receiver As _\r\n New Worker\r\n",
            "Private receiver As New Worker\r\nPublic Sub Run()\r\n",
        ] {
            let root = tempdir();
            let storage = tempdir();
            let client = format!(
                "Attribute VB_Name = \"Client\"\r\n{declaration}receiver.Apply\r\nEnd Sub\r\n"
            );
            let worker = b"Attribute VB_Name = \"Worker\"\r\nPublic Sub Apply()\r\nEnd Sub\r\n";
            if native {
                fs::write(
                    root.path().join("book.xlsm"),
                    fixture::typed_book(&[
                        ("Client", client.as_bytes(), false),
                        ("Worker", worker, true),
                    ]),
                )
                .unwrap();
            } else {
                fs::write(root.path().join("Client.bas"), &client).unwrap();
                fs::write(
                    root.path().join("Worker.cls"),
                    [b"VERSION 1.0 CLASS\r\n".as_slice(), worker].concat(),
                )
                .unwrap();
                fs::write(
                    root.path().join("project.vbp"),
                    "Type=Exe\nName=\"Project\"\nModule=Client; Client.bas\nClass=Worker; Worker.cls\n",
                )
                .unwrap();
            }
            let paths = rootlight_runtime::RuntimePaths::new(
                storage.path().join("state"),
                storage.path().join("runtime"),
            )
            .unwrap();
            paths.prepare_owner().unwrap();
            let mut service =
                FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
            let receipt = service.index_repository(root.path(), &deadline()).unwrap();
            assert_eq!(
                service.index_repository(root.path(), &deadline()).unwrap(),
                receipt
            );
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            for (syntax, name, kind) in [
                (
                    "vba.automatic_type_identifier.reference",
                    "Worker",
                    EntityKind::Class,
                ),
                ("vba.invoked_member.reference", "Apply", EntityKind::Method),
            ] {
                let target = document
                    .entities
                    .iter()
                    .find(|e| e.canonical_name == name && e.kind == kind)
                    .unwrap();
                let sites: Vec<_> = document
                    .occurrences
                    .iter()
                    .filter(|o| o.syntax_kind == syntax)
                    .collect();
                assert_eq!(sites.len(), 1, "native={native} {declaration} {syntax}");
                assert_eq!(
                    sites[0].target,
                    OccurrenceTarget::Resolved { symbol: target.id }
                );
                assert!(!document.skipped_regions.iter().any(|gap| {
                    gap.source == sites[0].source
                        && matches!(
                            gap.detail.as_str(),
                            "vba-type-target-unavailable" | "vba-member-target-unavailable"
                        )
                }));
                let proof = document
                    .provenance
                    .iter()
                    .find(|p| p.id == sites[0].provenance)
                    .unwrap();
                assert!(
                    proof
                        .input_sources
                        .iter()
                        .chain(&proof.evidence_sources)
                        .all(|s| s.generation() == receipt.generation)
                );
                assert!(document.relations.iter().any(|r| {
                    r.object == RelationEndpoint::Entity(target.id)
                        && r.predicate
                            == if kind == EntityKind::Class {
                                RelationPredicate::UsesType
                            } else {
                                RelationPredicate::Calls
                            }
                        && r.evidence.source.as_ref() == Some(&sites[0].source)
                }));
            }
            let rebuilt = service
                .prepare_repository_with_options(
                    root.path(),
                    FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(
                service
                    .publish_prepared(rebuilt, &deadline())
                    .unwrap()
                    .logical_snapshot,
                receipt.logical_snapshot
            );
            drop(service);
            let restored =
                FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
            assert_eq!(
                restored
                    .loaded_generation_snapshot(receipt.generation)
                    .unwrap()
                    .document(),
                document
            );
        }
    }
}

#[test]
fn expression_types_follow_project_sources_without_inventing_value_types() {
    for native in [false, true] {
        let root = tempdir();
        let storage = tempdir();
        let client = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Run(ByVal Worker As Long)\r\nDim receiver As Object\r\nSet receiver = New Worker\r\nIf TypeOf receiver Is Worker Then\r\nreceiver.Apply\r\nEnd If\r\nEnd Sub\r\n";
        let worker = b"Attribute VB_Name = \"Worker\"\r\nPublic Sub Apply()\r\nEnd Sub\r\n";
        if native {
            fs::write(
                root.path().join("book.xlsm"),
                fixture::typed_book(&[("Client", client, false), ("Worker", worker, true)]),
            )
            .unwrap();
        } else {
            fs::write(root.path().join("Client.bas"), client).unwrap();
            fs::write(
                root.path().join("Worker.cls"),
                [b"VERSION 1.0 CLASS\r\n".as_slice(), worker].concat(),
            )
            .unwrap();
            fs::write(
                root.path().join("project.vbp"),
                "Type=Exe\nName=\"Project\"\nModule=Client; Client.bas\nClass=Worker; Worker.cls\n",
            )
            .unwrap();
        }
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        drop(service);
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Worker")
            .unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.constructed_type_identifier.reference")
            .expect("New must retain its type reference");
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(site.role, OccurrenceRole::TypeUse);
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::UsesType
                    && edge.object == RelationEndpoint::Entity(target.id)
                    && edge.evidence.source.as_ref() == Some(&site.source))
        );
        assert!(!document.skipped_regions.iter().any(|gap| gap.source == site.source && gap.detail == "vba-type-target-unavailable"));
        let tested = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.tested_type_identifier.reference")
            .unwrap();
        assert_eq!(
            tested.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(tested.role, OccurrenceRole::TypeUse);
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::UsesType
                    && edge.object == RelationEndpoint::Entity(target.id)
                    && edge.evidence.source.as_ref() == Some(&tested.source))
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == tested.source
                    && gap.detail == "vba-type-target-unavailable")
        );
        let construction = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.construction.reference")
            .unwrap();
        assert_eq!(
            construction.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == construction.source
                    && gap.detail == "vba-construction-semantics-unavailable")
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.evidence.source.as_ref() == Some(&construction.source))
        );
        let member = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.invoked_member.reference")
            .unwrap();
        assert!(matches!(member.target, OccurrenceTarget::Unresolved { .. }));
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![site.source.clone(), tested.source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, b"Worker");
        assert_eq!(read.data.chunks[1].bytes, b"Worker");
        assert_eq!(tested.source.generation(), receipt.generation);
        assert_eq!(read.data.chunks[1].extracted_from.is_some(), native);
        assert_eq!(site.source.generation(), receipt.generation);
        assert_eq!(read.data.chunks[0].extracted_from.is_some(), native);
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
        assert!(receipt.logical_snapshot.is_some());
        assert_eq!(rebuilt.logical_snapshot, receipt.logical_snapshot);
    }
}

#[test]
fn construction_lifecycle_and_receivers_follow_source_mutations_after_reopen() {
    use rootlight_source::vba_project::ProjectModuleKind;

    for native in [false, true] {
        let root = tempdir();
        let storage = tempdir();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let client = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Run()\r\nDim item As Object\r\nSet item = New Worker\r\nWith New Worker\r\n.Touch\r\nEnd With\r\nitem.Touch\r\nEnd Sub\r\n";
        let mut previous = None;
        for (body, kind, expected, calls) in [
            ("", ProjectModuleKind::Class, true, false),
            (
                "Private Sub Class_Initialize()\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                true,
                true,
            ),
            (
                "Private Sub Class_Initialize()\r\nDim value As Long\r\nvalue = 2\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                true,
                true,
            ),
            (
                "Private Sub Class_Initialize\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                true,
                true,
            ),
            (
                "Private Sub cLaSs_InItIaLiZe _\r\n ( _\r\n )\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                true,
                true,
            ),
            (
                "Private Sub Class_Terminate()\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                true,
                false,
            ),
            (
                "#If Choice Then\r\nSub Other()\r\nEnd Sub\r\n#End If\r\n",
                ProjectModuleKind::Class,
                true,
                false,
            ),
            (
                "#If Choice Then\r\nSub Other()\r\nEnd Sub\r\n#End If\r\nPrivate Sub Class_Initialize()\r\n#If Choice Then\r\nDim value As Long\r\n#End If\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                true,
                true,
            ),
            (
                "Private Sub Other()\r\n' Sub Class_Initialize()\r\nDim text As String\r\ntext = \"Sub Class_Initialize()\"\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                true,
                false,
            ),
            (
                "Private Sub Class_Initialize(value As Long)\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                false,
                false,
            ),
            (
                "Private Sub Class_Initialize(Optional value As Long = 0)\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                false,
                false,
            ),
            (
                "Private Sub Class_Initialize(ParamArray values())\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                false,
                false,
            ),
            (
                "Private Function Class_Initialize() As Long\r\nEnd Function\r\n",
                ProjectModuleKind::Class,
                false,
                false,
            ),
            (
                "Private Property Get Class_Initialize() As Long\r\nEnd Property\r\n",
                ProjectModuleKind::Class,
                false,
                false,
            ),
            (
                "Private Class_Initialize As Long\r\n",
                ProjectModuleKind::Class,
                false,
                false,
            ),
            (
                "Private Sub Class_Initialize()\r\nEnd Sub\r\nPrivate Sub Class_Initialize()\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                false,
                false,
            ),
            (
                "#If HostChoice Then\r\nPrivate Sub Class_Initialize()\r\nEnd Sub\r\n#End If\r\n",
                ProjectModuleKind::Class,
                false,
                false,
            ),
            (
                "Private Sub Class_Initialize()\r\nEnd Sub\r\n",
                ProjectModuleKind::Document {
                    automation_version: 0,
                },
                false,
                false,
            ),
            (
                "Private Sub Class_Initialize()\r\nEnd Sub\r\n",
                ProjectModuleKind::Designer,
                false,
                false,
            ),
            (
                "Private Sub Class_Initialize()\r\nEnd Sub\r\n",
                ProjectModuleKind::Procedural,
                false,
                false,
            ),
            (
                "Private Sub Class_Initialize()\r\nEnd Sub\r\n",
                ProjectModuleKind::Class,
                true,
                true,
            ),
        ] {
            if !native
                && matches!(
                    kind,
                    ProjectModuleKind::Document { .. } | ProjectModuleKind::Designer
                )
            {
                continue;
            }
            let worker = format!(
                "Attribute VB_Name = \"Worker\"\r\n{body}Public Sub Touch()\r\nEnd Sub\r\n"
            );
            if native {
                fs::write(
                    root.path().join("book.xlsm"),
                    fixture::native_book(
                        &[
                            ("Client", client, ProjectModuleKind::Procedural),
                            ("Worker", worker.as_bytes(), kind),
                        ],
                        "\"Project\"",
                    ),
                )
                .unwrap();
            } else {
                fs::write(root.path().join("Client.bas"), client).unwrap();
                let class = kind == ProjectModuleKind::Class;
                fs::write(
                    root.path().join("Worker.cls"),
                    format!(
                        "{}{worker}",
                        if class { "VERSION 1.0 CLASS\r\n" } else { "" }
                    ),
                )
                .unwrap();
                fs::write(root.path().join("project.vbp"), format!(
                    "Type=Exe\nName=\"Project\"\nModule=Client; Client.bas\n{}=Worker; Worker.cls\n",
                    if class { "Class" } else { "Module" },
                )).unwrap();
            }
            let mut service =
                FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
            let receipt = service.index_repository(root.path(), &deadline()).unwrap();
            assert_ne!(Some(receipt.generation), previous);
            previous = Some(receipt.generation);
            assert_eq!(
                service.index_repository(root.path(), &deadline()).unwrap(),
                receipt
            );
            drop(service);
            let mut service =
                FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            let class = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "Worker"
                        && matches!(entity.kind, EntityKind::Class | EntityKind::Module)
                })
                .unwrap();
            let sites: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| site.syntax_kind == "vba.construction.reference")
                .collect();
            assert_eq!(sites.len(), 2);
            for site in &sites {
                assert_eq!(
                    site.target == OccurrenceTarget::Resolved { symbol: class.id },
                    expected,
                    "native={native} kind={kind:?} body={body:?}"
                );
                if !expected {
                    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
                }
                assert_eq!(
                    document
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.source == site.source
                            && gap.detail == "vba-construction-semantics-unavailable"),
                    !expected
                );
                let edges: Vec<_> = document
                    .relations
                    .iter()
                    .filter(|edge| edge.evidence.source.as_ref() == Some(&site.source))
                    .collect();
                assert_eq!(
                    edges
                        .iter()
                        .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                            && edge.object == RelationEndpoint::Entity(class.id))
                        .count(),
                    usize::from(expected)
                );
                let initializations: Vec<_> = edges
                    .iter()
                    .filter(|edge| edge.predicate == RelationPredicate::Calls)
                    .collect();
                assert_eq!(
                    initializations.len(),
                    usize::from(calls),
                    "native={native} body={body:?}"
                );
                if calls {
                    let initializer = document
                        .entities
                        .iter()
                        .find(|entity| {
                            entity
                                .canonical_name
                                .eq_ignore_ascii_case("Class_Initialize")
                                && entity.container == Some(ContainerRef::Entity(class.id))
                        })
                        .unwrap();
                    assert_eq!(
                        initializations[0].object,
                        RelationEndpoint::Entity(initializer.id)
                    );
                    let provenance = document
                        .provenance
                        .iter()
                        .find(|record| record.id == site.provenance)
                        .unwrap();
                    assert_eq!(
                        provenance.rule.as_deref(),
                        Some("classic-project-construction-v1.indexed_snapshot")
                    );
                    assert!(
                        provenance
                            .derivation_parents
                            .contains(&FactRef::Entity(initializer.id))
                    );
                    assert!(
                        provenance
                            .evidence_sources
                            .contains(initializer.evidence.source.as_ref().unwrap())
                    );
                }
            }
            let method = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "Touch"
                        && entity.container == Some(ContainerRef::Entity(class.id))
                })
                .unwrap();
            if calls {
                let initializer = document
                    .entities
                    .iter()
                    .find(|entity| {
                        entity
                            .canonical_name
                            .eq_ignore_ascii_case("Class_Initialize")
                            && entity.container == Some(ContainerRef::Entity(class.id))
                    })
                    .unwrap();
                let relationships = service
                    .symbol_relationships(
                        receipt.generation,
                        BTreeSet::from([initializer.id]),
                        vec![RelationFamily::Calls],
                        Some(RelationDirection::Inbound),
                        0,
                        25,
                        0,
                        &deadline(),
                    )
                    .unwrap();
                assert!(!relationships.data.truncated);
                for site in &sites {
                    assert!(
                        relationships
                            .data
                            .groups
                            .iter()
                            .flat_map(|group| &group.items)
                            .any(|edge| edge.source_refs.contains(&site.source))
                    );
                }
            }
            let members: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| site.syntax_kind == "vba.invoked_member.reference")
                .collect();
            assert_eq!(members.len(), 2);
            let with_start = client
                .windows(b".Touch".len())
                .position(|part| part == b".Touch")
                .unwrap();
            for site in members {
                let with = site.source.span().start_byte() == u64::try_from(with_start).unwrap();
                assert_eq!(
                    site.target == OccurrenceTarget::Resolved { symbol: method.id },
                    with && expected,
                    "native={native} body={body:?} site={site:?}"
                );
            }
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    sites.iter().map(|site| site.source.clone()).collect(),
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(read.data.chunks.len(), 2);
            for chunk in &read.data.chunks {
                assert_eq!(chunk.bytes, b"New Worker");
                assert_eq!(chunk.extracted_from.is_some(), native);
            }
            let prepared = service
                .prepare_repository_with_options(
                    root.path(),
                    FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                    &deadline(),
                )
                .unwrap();
            let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
            assert!(receipt.logical_snapshot.is_some());
            assert_eq!(rebuilt.logical_snapshot, receipt.logical_snapshot);
        }
    }
}

#[test]
fn expression_type_uses_do_not_declare_property_return_types() {
    for body in [
        "Set Item = New Worker",
        "If TypeOf candidate Is Worker Then\r\nSet Item = candidate\r\nEnd If",
    ] {
        let root = tempdir();
        let client = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Run(ByVal factory As Maker)\r\nfactory.Item.Apply\r\nEnd Sub\r\n";
        let maker = format!(
            "Attribute VB_Name = \"Maker\"\r\nPrivate candidate As Object\r\nPublic Property Get Item() As Object\r\n{body}\r\nEnd Property\r\n"
        );
        let worker = b"Attribute VB_Name = \"Worker\"\r\nPublic Sub Apply()\r\nEnd Sub\r\n";
        fs::write(
            root.path().join("book.xlsm"),
            fixture::typed_book(&[
                ("Client", client, false),
                ("Maker", maker.as_bytes(), true),
                ("Worker", worker, true),
            ]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(4).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::TypeUse
                    && site.enclosing.is_some_and(|owner| {
                        document
                            .entities
                            .iter()
                            .any(|entity| entity.id == owner && entity.kind == EntityKind::Property)
                    })
            })
            .unwrap();
        assert!(matches!(site.target, OccurrenceTarget::Resolved { .. }));
        let member = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.invoked_member.reference")
            .unwrap();
        assert!(
            matches!(member.target, OccurrenceTarget::Unresolved { .. }),
            "{body}: {:?}",
            member.target
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.evidence.source.as_ref() == Some(&member.source))
        );
    }
}

#[test]
fn constructed_object_types_reject_missing_ambiguous_and_nonclass_targets() {
    let root = tempdir();
    let mut service = FirstSliceService::new(4).unwrap();
    for scenario in [
        "missing",
        "record",
        "enum",
        "local-record",
        "local-enum",
        "collision",
        "qualified",
        "qualified-local",
    ] {
        let local = match scenario {
            "local-record" => "Private Type Worker\r\nValue As Long\r\nEnd Type\r\n",
            "local-enum" => "Private Enum Worker\r\nReady = 1\r\nEnd Enum\r\n",
            _ => "",
        };
        let name = match scenario {
            "qualified" => "Missing.Worker",
            "qualified-local" => "Worker.Unknown",
            _ => "Worker",
        };
        let client = format!(
            "Attribute VB_Name = \"Client\"\r\n{local}Public Sub Run()\r\nDim value As Object\r\nSet value = New {name}\r\nEnd Sub\r\n"
        );
        let exported = match scenario {
            "record" | "collision" => "Public Type Worker\r\nValue As Long\r\nEnd Type\r\n",
            "enum" => "Public Enum Worker\r\nReady = 1\r\nEnd Enum\r\n",
            _ => "",
        };
        let globals = format!("Attribute VB_Name = \"Globals\"\r\n{exported}");
        let worker = b"Attribute VB_Name = \"Worker\"\r\nPublic Sub Apply()\r\nEnd Sub\r\n";
        let mut modules = vec![
            ("Client", client.as_bytes(), false),
            ("Globals", globals.as_bytes(), false),
        ];
        if !matches!(scenario, "missing" | "record" | "enum") {
            modules.push(("Worker", worker, true));
        }
        fs::write(root.path().join("book.xlsm"), fixture::typed_book(&modules)).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind.contains("constructed_type_identifier"))
            .collect();
        assert!(!sites.is_empty(), "{scenario}");
        for site in sites {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{scenario}"
            );
            assert!(document.skipped_regions.iter().any(
                |gap| gap.source == site.source && gap.detail == "vba-type-target-unavailable"
            ));
        }
    }
}

#[test]
fn automatic_object_types_do_not_invent_class_or_member_bindings() {
    let root = tempdir();
    let mut service = FirstSliceService::new(4).unwrap();
    for scenario in [
        "missing",
        "record",
        "enum",
        "collision",
        "local-record",
        "local-enum",
        "private-method",
        "parameter",
        "array",
        "shadowed-receiver",
    ] {
        let local = match scenario {
            "local-record" => "Private Type Worker\r\nApply As Long\r\nEnd Type\r\n",
            "local-enum" => "Private Enum Worker\r\nApply = 1\r\nEnd Enum\r\n",
            _ => "",
        };
        let declaration = match scenario {
            "parameter" => "Public Sub Run(ByVal receiver As New Worker)\r\n",
            "array" => "Public Sub Run()\r\nDim receiver() As New Worker\r\n",
            "shadowed-receiver" => {
                "Private receiver As New Worker\r\nPublic Sub Run(ByVal receiver As Variant)\r\n"
            }
            _ => "Public Sub Run()\r\nDim receiver As New Worker\r\n",
        };
        let client = format!(
            "Attribute VB_Name = \"Client\"\r\n{local}{declaration}receiver.Apply\r\nEnd Sub\r\n"
        );
        let exported = match scenario {
            "record" | "collision" => "Public Type Worker\r\nApply As Long\r\nEnd Type\r\n",
            "enum" => "Public Enum Worker\r\nApply = 1\r\nEnd Enum\r\n",
            _ => "",
        };
        let globals = format!("Attribute VB_Name = \"Globals\"\r\n{exported}");
        let access = if scenario == "private-method" {
            "Private"
        } else {
            "Public"
        };
        let worker =
            format!("Attribute VB_Name = \"Worker\"\r\n{access} Sub Apply()\r\nEnd Sub\r\n");
        let mut modules = vec![
            ("Client", client.as_bytes(), false),
            ("Globals", globals.as_bytes(), false),
        ];
        if !matches!(scenario, "missing" | "record" | "enum") {
            modules.push(("Worker", worker.as_bytes(), true));
        }
        fs::write(root.path().join("book.xlsm"), fixture::typed_book(&modules)).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let client_path = derived_path("book.xlsm", "Client", ModuleKind::Procedural).unwrap();
        let client_file = document
            .files
            .iter()
            .find(|f| f.path == client_path.as_str())
            .unwrap();
        let member = document
            .occurrences
            .iter()
            .find(|o| o.file == client_file.id && o.syntax_kind == "vba.invoked_member.reference")
            .unwrap();
        assert!(
            matches!(member.target, OccurrenceTarget::Unresolved { .. }),
            "{scenario}: {:?}",
            member.target
        );
        let type_site = document
            .occurrences
            .iter()
            .find(|o| {
                o.file == client_file.id && o.syntax_kind.contains("automatic_type_identifier")
            })
            .unwrap();
        if matches!(scenario, "private-method" | "shadowed-receiver") {
            assert!(
                matches!(type_site.target, OccurrenceTarget::Resolved { .. }),
                "{scenario}"
            );
        } else {
            assert!(
                matches!(type_site.target, OccurrenceTarget::Unresolved { .. }),
                "{scenario}: {:?}",
                type_site.target
            );
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == type_site.source
                        && gap.detail == "vba-type-target-unavailable"),
                "{scenario}"
            );
        }
        assert!(
            !document
                .relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Calls
                    && r.evidence.source.as_ref() == Some(&member.source)),
            "{scenario}"
        );
    }
}

#[test]
fn native_predeclared_lookup_respects_enum_namespace_tiers() {
    let root = tempdir();
    let surface = b"Attribute VB_Name = \"Surface\"\r\nAttribute VB_PredeclaredId = True\r\nPublic Sub Apply()\r\nEnd Sub\r\n";
    let mut service = FirstSliceService::new(4).unwrap();
    for (access, kind, local, expected) in [
        ("Public", ProjectModuleKind::Procedural, "", false),
        ("Private", ProjectModuleKind::Procedural, "", true),
        ("Public", ProjectModuleKind::Class, "", true),
        (
            "Private",
            ProjectModuleKind::Procedural,
            "Private Enum FirstState\r\nSurface = 1\r\nEnd Enum\r\n",
            false,
        ),
        (
            "Private",
            ProjectModuleKind::Procedural,
            "Private Enum FirstState\r\nSurface = 1\r\nEnd Enum\r\nPrivate Enum SecondState\r\nSurface = 2\r\nEnd Enum\r\n",
            false,
        ),
    ] {
        let client = format!(
            "Attribute VB_Name = \"Client\"\r\n{local}Public Sub Run()\r\nSurface.Apply\r\nEnd Sub\r\n"
        );
        let globals = format!(
            "Attribute VB_Name = \"Globals\"\r\n{access} Enum Phase\r\nSurface = 1\r\nEnd Enum\r\n"
        );
        fs::write(
            root.path().join("book.xlsm"),
            fixture::native_book(
                &[
                    ("Client", client.as_bytes(), ProjectModuleKind::Procedural),
                    ("Surface", surface, ProjectModuleKind::Class),
                    ("Globals", globals.as_bytes(), kind),
                ],
                "\"Project\"",
            ),
        )
        .unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let source = read_module(
            &service,
            receipt.generation,
            derived_path("book.xlsm", "Client", ModuleKind::Procedural)
                .unwrap()
                .as_str(),
            client.as_bytes(),
        );
        let site = document
            .occurrences
            .iter()
            .find(|o| {
                o.file == source.span().file() && o.syntax_kind == "vba.invoked_member.reference"
            })
            .unwrap();
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            expected,
            "{access} {kind:?} {local}"
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|r| r.subject == RelationEndpoint::Occurrence(site.id)
                    && r.predicate == RelationPredicate::Calls)
                .count(),
            usize::from(expected)
        );
    }
}

#[test]
fn native_predeclared_members_follow_effective_attributes_and_lexical_shadowing() {
    let storage = tempdir();
    let root = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let client = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Run()\r\nSurface.Apply\r\nEnd Sub\r\nPublic Sub Shadow(ByVal Surface As Object)\r\nSurface.Apply\r\nEnd Sub\r\nPublic Sub Typed(ByVal Surface As Worker)\r\nSurface.Apply\r\nEnd Sub\r\n";
    let worker = b"Attribute VB_Name = \"Worker\"\r\nPublic Sub Apply()\r\nEnd Sub\r\n";
    let enabled = "Attribute VB_PredeclaredId = True\r\n";
    let fixed =
        format!("Attribute VB_Name = \"Surface\"\r\n{enabled}Public Sub Apply()\r\nEnd Sub\r\n");
    let globals = b"Attribute VB_Name = \"Globals\"\r\nPublic Other As Object\r\n";
    let collision = b"Attribute VB_Name = \"Globals\"\r\nPublic Surface As Object\r\n";
    fs::write(
        root.path().join("fixed.xlsm"),
        fixture::native_book(
            &[
                ("Client", client, ProjectModuleKind::Procedural),
                ("Surface", fixed.as_bytes(), ProjectModuleKind::Class),
                ("Worker", worker, ProjectModuleKind::Class),
                ("Globals", globals, ProjectModuleKind::Procedural),
            ],
            "\"Project\"",
        ),
    )
    .unwrap();
    let mut previous = None;
    for (attributes, kind, public, conflicting, expected) in [
        ("", ProjectModuleKind::Class, true, false, false),
        (enabled, ProjectModuleKind::Class, true, false, true),
        (
            "Attribute VB_PredeclaredId = True\r\nAttribute VB_PredeclaredId = False\r\n",
            ProjectModuleKind::Class,
            true,
            false,
            false,
        ),
        (
            "Attribute VB_PredeclaredId = False\r\nAttribute VB_PredeclaredId = True\r\n",
            ProjectModuleKind::Document {
                automation_version: 0,
            },
            true,
            false,
            true,
        ),
        (enabled, ProjectModuleKind::Designer, true, false, true),
        (enabled, ProjectModuleKind::Designer, false, false, false),
        (enabled, ProjectModuleKind::Class, true, true, false),
        (enabled, ProjectModuleKind::Class, true, false, true),
    ] {
        let body = format!(
            "Attribute VB_Name = \"Surface\"\r\n{attributes}{} Sub Apply()\r\nEnd Sub\r\n",
            if public { "Public" } else { "Private" }
        );
        fs::write(
            root.path().join("book.xlsm"),
            fixture::native_book(
                &[
                    ("Client", client, ProjectModuleKind::Procedural),
                    ("Surface", body.as_bytes(), kind),
                    ("Worker", worker, ProjectModuleKind::Class),
                    (
                        "Globals",
                        if conflicting {
                            collision.as_slice()
                        } else {
                            globals.as_slice()
                        },
                        ProjectModuleKind::Procedural,
                    ),
                ],
                "\"Project\"",
            ),
        )
        .unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(receipt.extracted_files, 8);
        assert_ne!(Some(receipt.generation), previous);
        previous = Some(receipt.generation);
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        for container in ["book.xlsm", "fixed.xlsm"] {
            let source = read_module(
                &service,
                receipt.generation,
                derived_path(container, "Surface", ModuleKind::Object)
                    .unwrap()
                    .as_str(),
                if container == "book.xlsm" {
                    body.as_bytes()
                } else {
                    fixed.as_bytes()
                },
            );
            let client_source = read_module(
                &service,
                receipt.generation,
                derived_path(container, "Client", ModuleKind::Procedural)
                    .unwrap()
                    .as_str(),
                client,
            );
            let worker_source = read_module(
                &service,
                receipt.generation,
                derived_path(container, "Worker", ModuleKind::Object)
                    .unwrap()
                    .as_str(),
                worker,
            );
            let method = |file| {
                document
                    .entities
                    .iter()
                    .find(|e| {
                        e.canonical_name == "Apply"
                            && e.evidence
                                .source
                                .as_ref()
                                .is_some_and(|s| s.span().file() == file)
                    })
                    .unwrap()
            };
            let target = method(source.span().file());
            let typed_target = method(worker_source.span().file());
            let mut sites: Vec<_> = document
                .occurrences
                .iter()
                .filter(|o| {
                    o.file == client_source.span().file()
                        && o.syntax_kind == "vba.invoked_member.reference"
                })
                .collect();
            sites.sort_by_key(|o| o.source.span().start_byte());
            assert_eq!(sites.len(), 3);
            let targets = [
                (expected || container == "fixed.xlsm").then_some(target.id),
                None,
                Some(typed_target.id),
            ];
            for (site, expected) in sites.iter().zip(targets) {
                assert_eq!(
                    match site.target {
                        OccurrenceTarget::Resolved { symbol } => Some(symbol),
                        _ => None,
                    },
                    expected,
                    "{container}: {}",
                    site.source.span().start_byte()
                );
                let edges: Vec<_> = document
                    .relations
                    .iter()
                    .filter(|r| {
                        r.predicate == RelationPredicate::Calls
                            && r.subject == RelationEndpoint::Occurrence(site.id)
                    })
                    .collect();
                assert_eq!(edges.len(), usize::from(expected.is_some()));
                if let Some(expected) = expected {
                    assert_eq!(edges[0].object, RelationEndpoint::Entity(expected));
                }
            }
            if targets[0].is_some() {
                let provenance = document
                    .provenance
                    .iter()
                    .find(|p| p.id == sites[0].provenance)
                    .unwrap();
                let bytes = if container == "book.xlsm" {
                    body.as_bytes()
                } else {
                    fixed.as_bytes()
                };
                assert!(provenance.evidence_sources.iter().any(|s| s.span().file()
                    == source.span().file()
                    && bytes.get(
                        usize::try_from(s.span().start_byte()).unwrap()
                            ..usize::try_from(s.span().end_byte()).unwrap()
                    ) == Some(b"True".as_slice())));
            }
            let run = document
                .entities
                .iter()
                .find(|e| {
                    e.canonical_name == "Run"
                        && e.evidence
                            .source
                            .as_ref()
                            .is_some_and(|s| s.span().file() == client_source.span().file())
                })
                .unwrap();
            let relationships = service
                .symbol_relationships(
                    receipt.generation,
                    BTreeSet::from([run.id]),
                    vec![RelationFamily::Calls],
                    Some(RelationDirection::Outbound),
                    0,
                    16,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert!(!relationships.data.truncated);
            let refs = relationships
                .data
                .groups
                .iter()
                .flat_map(|g| &g.items)
                .flat_map(|i| i.source_refs.iter().cloned())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                refs,
                if targets[0].is_some() {
                    BTreeSet::from([sites[0].source.clone()])
                } else {
                    BTreeSet::new()
                }
            );
        }
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            service
                .publish_prepared(prepared, &deadline())
                .unwrap()
                .logical_snapshot,
            receipt.logical_snapshot
        );
        drop(service);
        let restored = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            restored
                .loaded_generation_snapshot(receipt.generation)
                .unwrap()
                .document(),
            document
        );
    }
}

#[test]
fn native_extensions_resolve_declared_members_without_inventing_host_context() {
    let storage = tempdir();
    let root = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let client = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Run(ByVal value As Surface, ByVal worker As Worker)\r\nvalue.Apply\r\nworker.Perform\r\nvalue.HostOnly\r\nSurface.Apply\r\nEnd Sub\r\n";
    let public = b"Attribute VB_Name = \"Surface\"\r\nAttribute VB_PredeclaredId = False\r\nPublic Sub Apply()\r\nEnd Sub\r\n";
    let private = b"Attribute VB_Name = \"Surface\"\r\nAttribute VB_PredeclaredId = False\r\nPrivate Sub Apply()\r\nEnd Sub\r\n";
    let worker = b"Attribute VB_Name = \"Worker\"\r\nPublic Sub Perform()\r\nEnd Sub\r\n";
    let document_kind = ProjectModuleKind::Document {
        automation_version: 0,
    };
    fs::write(
        root.path().join("fixed.xlsm"),
        fixture::native_book(
            &[
                ("Client", client, ProjectModuleKind::Procedural),
                ("Surface", public, document_kind),
                ("Worker", worker, ProjectModuleKind::Class),
            ],
            "\"Project\"",
        ),
    )
    .unwrap();
    let mut previous = None;
    for (kind, body, callable) in [
        (ProjectModuleKind::Class, public.as_slice(), true),
        (document_kind, public, true),
        (ProjectModuleKind::Designer, public, true),
        (ProjectModuleKind::Designer, private, false),
        (ProjectModuleKind::Class, public, true),
    ] {
        fs::write(
            root.path().join("book.xlsm"),
            fixture::native_book(
                &[
                    ("Client", client, ProjectModuleKind::Procedural),
                    ("Surface", body, kind),
                    ("Worker", worker, ProjectModuleKind::Class),
                ],
                "\"Project\"",
            ),
        )
        .unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(receipt.extracted_files, 6);
        assert_ne!(Some(receipt.generation), previous);
        previous = Some(receipt.generation);
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let warnings: Vec<_> = document
            .diagnostics
            .iter()
            .filter(|d| d.message.starts_with("vba extension context incomplete:"))
            .collect();
        let active_extension = kind != ProjectModuleKind::Class;
        assert_eq!(warnings.len(), 1 + usize::from(active_extension));
        for container in ["book.xlsm", "fixed.xlsm"] {
            let surface_path = derived_path(container, "Surface", ModuleKind::Object).unwrap();
            let source = read_module(
                &service,
                receipt.generation,
                surface_path.as_str(),
                if container == "book.xlsm" {
                    body
                } else {
                    public
                },
            );
            let client_path = derived_path(container, "Client", ModuleKind::Procedural).unwrap();
            let client_source =
                read_module(&service, receipt.generation, client_path.as_str(), client);
            let surface = document
                .entities
                .iter()
                .find(|e| {
                    e.kind == EntityKind::Class
                        && e.canonical_name == "Surface"
                        && e.evidence
                            .source
                            .as_ref()
                            .is_some_and(|s| s.span().file() == source.span().file())
                })
                .unwrap();
            let method = document
                .entities
                .iter()
                .find(|e| {
                    e.canonical_name == "Apply"
                        && e.evidence
                            .source
                            .as_ref()
                            .is_some_and(|s| s.span().file() == source.span().file())
                })
                .unwrap();
            let type_site = document
                .occurrences
                .iter()
                .find(|o| {
                    o.file == client_source.span().file()
                        && o.syntax_kind == "vba.direct_type_identifier.reference"
                        && &client[usize::try_from(o.source.span().start_byte()).unwrap()
                            ..usize::try_from(o.source.span().end_byte()).unwrap()]
                            == b"Surface"
                })
                .unwrap();
            assert_eq!(
                type_site.target,
                OccurrenceTarget::Resolved { symbol: surface.id }
            );
            let mut calls = BTreeSet::new();
            for (text, expected) in [
                ("value.Apply", callable || container == "fixed.xlsm"),
                ("worker.Perform", true),
                ("value.HostOnly", false),
                ("Surface.Apply", false),
            ] {
                let begin = u64::try_from(std::str::from_utf8(client).unwrap().find(text).unwrap())
                    .unwrap();
                let site = document
                    .occurrences
                    .iter()
                    .find(|o| {
                        o.file == client_source.span().file()
                            && o.syntax_kind == "vba.invoked_member.reference"
                            && o.source.span().start_byte() == begin
                    })
                    .unwrap();
                assert_eq!(
                    matches!(site.target, OccurrenceTarget::Resolved { .. }),
                    expected,
                    "{container} {text}"
                );
                let edges: Vec<_> = document
                    .relations
                    .iter()
                    .filter(|r| {
                        r.predicate == RelationPredicate::Calls
                            && r.subject == RelationEndpoint::Occurrence(site.id)
                    })
                    .collect();
                assert_eq!(edges.len(), usize::from(expected), "{container} {text}");
                if expected {
                    calls.insert(site.source.clone());
                    if text == "value.Apply" {
                        assert_eq!(
                            site.target,
                            OccurrenceTarget::Resolved { symbol: method.id }
                        );
                        assert_eq!(edges[0].object, RelationEndpoint::Entity(method.id));
                    }
                }
            }
            let run = document
                .entities
                .iter()
                .find(|e| {
                    e.canonical_name == "Run"
                        && e.evidence
                            .source
                            .as_ref()
                            .is_some_and(|s| s.span().file() == client_source.span().file())
                })
                .unwrap();
            let relationships = service
                .symbol_relationships(
                    receipt.generation,
                    BTreeSet::from([run.id]),
                    vec![RelationFamily::Calls],
                    Some(RelationDirection::Outbound),
                    0,
                    16,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert!(!relationships.data.truncated);
            assert_eq!(
                relationships
                    .data
                    .groups
                    .iter()
                    .flat_map(|g| &g.items)
                    .flat_map(|i| i.source_refs.iter().cloned())
                    .collect::<BTreeSet<_>>(),
                calls
            );
            let scoped: Vec<_> = warnings
                .iter()
                .filter(|d| {
                    d.source
                        .as_ref()
                        .is_some_and(|s| s.span().file() == source.span().file())
                })
                .collect();
            let extension = container == "fixed.xlsm" || active_extension;
            if extension {
                assert_eq!(
                    service
                        .source_file_coverage(receipt.generation, source.span().file())
                        .unwrap()
                        .status,
                    CoverageStatus::Unknown
                );
            }
            for coverage in document
                .coverage_records
                .iter()
                .filter(|c| c.scope == CoverageScope::File(source.span().file()))
            {
                if matches!(
                    coverage.domain,
                    IrFactDomain::Files | IrFactDomain::Entities
                ) {
                    assert_eq!(coverage.status, CoverageStatus::Complete);
                    assert_eq!(coverage.discovered, coverage.indexed);
                    assert_eq!(coverage.skipped, 0);
                }
                if matches!(
                    coverage.domain,
                    IrFactDomain::Occurrences | IrFactDomain::Relations
                ) {
                    assert_eq!(coverage.status == CoverageStatus::Unknown, extension);
                }
            }
            assert_eq!(
                scoped.len(),
                usize::from(container == "fixed.xlsm" || active_extension)
            );
            for diagnostic in scoped {
                assert_eq!(diagnostic.coverage_effect, CoverageStatus::Unknown);
                assert_eq!(diagnostic.source.as_ref(), Some(&source));
            }
        }
        let rebuilt = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            service
                .publish_prepared(rebuilt, &deadline())
                .unwrap()
                .logical_snapshot,
            receipt.logical_snapshot
        );
        drop(service);
        let restored = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            restored
                .loaded_generation_snapshot(receipt.generation)
                .unwrap()
                .document(),
            document
        );
    }
}

#[test]
fn native_project_links_follow_container_membership_and_metadata_after_restart() {
    let storage = tempdir();
    let root = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let client = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Run(ByVal value As Worker)\r\nvalue.Apply\r\nEnd Sub\r\n";
    let class = b"Attribute VB_Name = \"Worker\"\r\nPublic Sub Apply()\r\nEnd Sub\r\n";
    fs::write(
        root.path().join("fixed.xlsm"),
        fixture::typed_book(&[("Client", client, false), ("Worker", class, true)]),
    )
    .unwrap();
    let mut previous = None;
    for (project_name, sibling, worker, linked, callable) in [
        ("\"Project\"", SECOND, class.as_slice(), true, true),
        ("\"Different\"", SECOND, class, false, false),
        ("\"Project\"", FIRST, class, false, false),
        (
            "\"Project\"",
            SECOND,
            b"Attribute VB_Name = \"Worker\"\r\nPrivate Sub Apply()\r\nEnd Sub\r\n",
            true,
            false,
        ),
        ("\"Project\"", SECOND, class, true, true),
    ] {
        fs::write(
            root.path().join("book.xlsm"),
            fixture::book_with_project_value(
                &[
                    ("Client", client, false),
                    ("Worker", worker, true),
                    ("Beta", sibling, false),
                ],
                project_name,
            ),
        )
        .unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(receipt.extracted_files, 5);
        assert_ne!(previous, Some(receipt.generation));
        previous = Some(receipt.generation);
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        for container in ["book.xlsm", "fixed.xlsm"] {
            let client_path = derived_path(container, "Client", ModuleKind::Procedural).unwrap();
            let class_path = derived_path(container, "Worker", ModuleKind::Object).unwrap();
            let source = read_module(&service, receipt.generation, client_path.as_str(), client);
            let target_file = document
                .files
                .iter()
                .find(|f| f.path == class_path.as_str())
                .unwrap();
            for (syntax, name, expected) in [
                ("vba.direct_type_identifier.reference", "Worker", linked),
                ("vba.invoked_member.reference", "Apply", callable),
            ] {
                let sites: Vec<_> = document
                    .occurrences
                    .iter()
                    .filter(|o| o.file == source.span().file() && o.syntax_kind == syntax)
                    .collect();
                assert_eq!(sites.len(), 1, "{container} {syntax}");
                let target = document
                    .entities
                    .iter()
                    .find(|e| {
                        e.canonical_name == name
                            && e.evidence
                                .source
                                .as_ref()
                                .is_some_and(|s| s.span().file() == target_file.id)
                    })
                    .unwrap();
                if expected || container == "fixed.xlsm" {
                    assert_eq!(
                        sites[0].target,
                        OccurrenceTarget::Resolved { symbol: target.id },
                        "{container} {syntax}"
                    );
                    let explained = service
                        .symbol_explain(receipt.generation, target.id, &deadline())
                        .unwrap();
                    assert_eq!(explained.data.entity.id, target.id);
                    assert_eq!(
                        explained
                            .data
                            .entity
                            .evidence
                            .source
                            .as_ref()
                            .unwrap()
                            .span()
                            .file(),
                        target_file.id
                    );
                } else {
                    assert!(
                        matches!(sites[0].target, OccurrenceTarget::Unresolved { .. }),
                        "{container} {syntax}"
                    );
                }
            }
            let container_file = document.files.iter().find(|f| f.path == container).unwrap();
            let roots: Vec<_> = document
                .provenance
                .iter()
                .filter(|p| {
                    p.rule.as_deref() == Some("native-classic-project-membership-v1.leaf")
                        && p.input_sources
                            .iter()
                            .chain(&p.evidence_sources)
                            .any(|s| s.span().file() == container_file.id)
                })
                .collect();
            assert_eq!(!roots.is_empty(), linked || container == "fixed.xlsm");
            for provenance in roots {
                assert_eq!(provenance.language, "vba");
                for reference in provenance
                    .input_sources
                    .iter()
                    .chain(&provenance.evidence_sources)
                {
                    assert_eq!(reference.generation(), receipt.generation);
                    let file = document
                        .files
                        .iter()
                        .find(|f| f.id == reference.span().file())
                        .unwrap();
                    assert!(
                        file.path == container
                            || file.path.starts_with(&format!("{container}/@vba/"))
                    );
                }
            }
        }
        let rebuilt = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        let rebuilt = service.publish_prepared(rebuilt, &deadline()).unwrap();
        assert_eq!(rebuilt.logical_snapshot, receipt.logical_snapshot);
        drop(service);
        let restored = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            restored
                .loaded_generation_snapshot(receipt.generation)
                .unwrap()
                .document(),
            document
        );
    }
}

#[test]
fn native_project_resolution_survives_duplicate_private_types_in_conditional_branches() {
    let storage = tempdir();
    let root = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let client = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Run()\r\nDim receiver As New Worker\r\nreceiver.Apply\r\nDim result As Long\r\nresult = Options.Combine(1)\r\nEnd Sub\r\n";
    let worker = b"Attribute VB_Name = \"Worker\"\r\nPublic Sub Apply()\r\nEnd Sub\r\n";
    let options = b"Attribute VB_Name = \"Options\"\r\n#If VBA7 Then\r\nPrivate Type Configuration\r\nValue As Long\r\nEnd Type\r\n#Else\r\nPrivate Type Configuration\r\nValue As Integer\r\nEnd Type\r\n#End If\r\nPublic Function Combine(ByVal value As Long) As Long\r\nCombine = value + 1\r\nEnd Function\r\n";
    fs::write(
        root.path().join("book.xlsm"),
        fixture::book_with_project_value(
            &[
                ("Client", client, false),
                ("Worker", worker, true),
                ("Options", options, false),
            ],
            "\"Project\"",
        ),
    )
    .unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(
        service.index_repository(root.path(), &deadline()).unwrap(),
        receipt
    );
    let document = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap()
        .document()
        .clone();
    let client_path = derived_path("book.xlsm", "Client", ModuleKind::Procedural).unwrap();
    let worker_path = derived_path("book.xlsm", "Worker", ModuleKind::Object).unwrap();
    let options_path = derived_path("book.xlsm", "Options", ModuleKind::Procedural).unwrap();
    let client_source = read_module(&service, receipt.generation, client_path.as_str(), client);
    let worker_file = document
        .files
        .iter()
        .find(|file| file.path == worker_path.as_str())
        .unwrap();
    let options_file = document
        .files
        .iter()
        .find(|file| file.path == options_path.as_str())
        .unwrap();
    for (syntax, name) in [
        ("vba.automatic_type_identifier.reference", "Worker"),
        ("vba.invoked_member.reference", "Apply"),
    ] {
        let expected_source: &[u8] = if name == "Apply" {
            b"receiver.Apply"
        } else {
            b"Worker"
        };
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                if site.file != client_source.span().file() || site.syntax_kind != syntax {
                    return false;
                }
                let span = site.source.span();
                let start = usize::try_from(span.start_byte()).unwrap();
                let end = usize::try_from(span.end_byte()).unwrap();
                client.get(start..end) == Some(expected_source)
            })
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == worker_file.id)
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(site.source.generation(), receipt.generation);
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Struct
                    && entity.canonical_name == "Configuration"
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == options_file.id)
            })
            .count(),
        2
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .all(|gap| gap.domain != IrFactDomain::Entities
                || gap.source.span().file() != options_file.id)
    );
    let qualified_call = document
        .occurrences
        .iter()
        .find(|site| {
            if site.file != client_source.span().file()
                || site.syntax_kind != "vba.invoked_get_member.reference"
            {
                return false;
            }
            let span = site.source.span();
            let start = usize::try_from(span.start_byte()).unwrap();
            let end = usize::try_from(span.end_byte()).unwrap();
            client.get(start..end) == Some(b"Options.Combine".as_slice())
        })
        .unwrap();
    let module_target = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Function
                && entity.canonical_name == "Combine"
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == options_file.id)
        })
        .unwrap();
    assert_eq!(
        qualified_call.target,
        OccurrenceTarget::Resolved {
            symbol: module_target.id
        }
    );
    assert!(document.relations.iter().any(|edge| {
        edge.subject == RelationEndpoint::Occurrence(qualified_call.id)
            && edge.object == RelationEndpoint::Entity(module_target.id)
            && edge.predicate == RelationPredicate::Calls
            && edge.evidence.source.as_ref() == Some(&qualified_call.source)
    }));
    let rebuilt = service
        .prepare_repository_with_options(
            root.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        service
            .publish_prepared(rebuilt, &deadline())
            .unwrap()
            .logical_snapshot,
        receipt.logical_snapshot
    );
    drop(service);
    let restored = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        restored
            .loaded_generation_snapshot(receipt.generation)
            .unwrap()
            .document(),
        &document
    );
}

#[test]
fn native_project_callback_references_require_the_same_container() {
    for class in [false, true] {
        assert_native_callback_container("Notify", "vba.procedure_pointer.reference", class);
    }
}

#[test]
fn native_project_qualified_callback_references_require_the_same_container() {
    for class in [false, true] {
        assert_native_callback_container(
            "[Callbacks].Notify",
            "vba.qualified_procedure_pointer.reference",
            class,
        );
    }
}

fn assert_native_callback_container(spelling: &str, syntax: &str, client_class: bool) {
    for (access, class, foreign, local, expected) in [
        ("Public", false, false, "", true),
        ("Private", false, false, "", false),
        ("Public", true, false, "", false),
        ("Public", false, true, "", false),
        (
            "Public",
            false,
            false,
            "Private Sub Callbacks()\r\nEnd Sub\r\n",
            syntax == "vba.procedure_pointer.reference",
        ),
    ] {
        let client = format!(
            "Attribute VB_Name = \"Client\"\r\n{local}Private Declare PtrSafe Sub Register Lib \"callbacks\" (ByVal address As LongPtr)\r\nSub Run()\r\nDim Notify As Long\r\nCall Register(AddressOf {spelling})\r\nEnd Sub\r\n"
        );
        let storage = tempdir();
        let root = tempdir();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let callback =
            format!("Attribute VB_Name = \"Callbacks\"\r\n{access} Sub Notify()\r\nEnd Sub\r\n");
        let mut modules = vec![("Client", client.as_bytes(), client_class)];
        modules.push(("Unused", b"Attribute VB_Name = \"Unused\"\r\n#If Choice Then\r\nPrivate Sub Hidden()\r\nEnd Sub\r\n#End If\r\n", true));
        if foreign {
            fs::write(
                root.path().join("other.xlsm"),
                fixture::typed_book(&[("Callbacks", callback.as_bytes(), class)]),
            )
            .unwrap();
        } else {
            modules.push(("Callbacks", callback.as_bytes(), class));
        }
        fs::write(root.path().join("book.xlsm"), fixture::typed_book(&modules)).unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        let before = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap()
            .document()
            .clone();
        drop(service);
        let service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        assert_eq!(document, &before);
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == syntax)
            .unwrap();
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            expected,
            "{access} class={class} foreign={foreign}"
        );
        assert_eq!(site.role, rootlight_ir::OccurrenceRole::Reference);
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == site.source
                    && gap.detail == "vba-procedure-pointer-target-unavailable"),
            !expected
        );
        assert!(!document.relations.iter().any(|relation| relation.subject
            == RelationEndpoint::Occurrence(site.id)
            && relation.predicate == RelationPredicate::Calls));
        if let OccurrenceTarget::Resolved { symbol } = site.target {
            let path = derived_path("book.xlsm", "Callbacks", ModuleKind::Procedural).unwrap();
            let target = document
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap();
            assert!(document.files.iter().any(|file| file.path == path.as_str()
                && target.evidence.source.as_ref().unwrap().span().file() == file.id));
            assert!(document.relations.iter().any(|relation| relation.subject
                == RelationEndpoint::Occurrence(site.id)
                && relation.object == RelationEndpoint::Entity(symbol)
                && relation.predicate == RelationPredicate::RefersTo
                && relation.evidence.source.as_ref() == Some(&site.source)));
            assert!(
                document
                    .provenance
                    .iter()
                    .any(|record| record.rule.as_deref()
                        == Some("native-classic-project-membership-v1.leaf"))
            );
        }
        for coverage in &document.coverage_records {
            assert_eq!(coverage.discovered, coverage.indexed + coverage.skipped);
        }
    }
}

#[test]
fn native_project_module_calls_require_public_targets_and_unshadowed_names() {
    for (visibility, local, foreign, expected) in [
        ("Public", "", false, true),
        ("Private", "", false, false),
        ("Public", "Dim Utility As Object\r\n", false, false),
        ("Public", "", true, false),
    ] {
        let storage = tempdir();
        let root = tempdir();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let client = format!(
            "Attribute VB_Name = \"Client\"\r\nPublic Sub Run()\r\n{local}Dim result As Long\r\nresult = Utility.Compute(1)\r\nEnd Sub\r\n"
        );
        let utility = format!(
            "Attribute VB_Name = \"Utility\"\r\n{visibility} Function Compute(ByVal value As Long) As Long\r\nCompute = value + 1\r\nEnd Function\r\n"
        );
        if foreign {
            fs::write(
                root.path().join("book.xlsm"),
                fixture::book_with_project_value(
                    &[("Client", client.as_bytes(), false)],
                    "\"Project\"",
                ),
            )
            .unwrap();
            fs::write(
                root.path().join("other.xlsm"),
                fixture::book_with_project_value(
                    &[("Utility", utility.as_bytes(), false)],
                    "\"Project\"",
                ),
            )
            .unwrap();
        } else {
            fs::write(
                root.path().join("book.xlsm"),
                fixture::book_with_project_value(
                    &[
                        ("Client", client.as_bytes(), false),
                        ("Utility", utility.as_bytes(), false),
                    ],
                    "\"Project\"",
                ),
            )
            .unwrap();
        }
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let client_path = derived_path("book.xlsm", "Client", ModuleKind::Procedural).unwrap();
        let client_file = document
            .files
            .iter()
            .find(|file| file.path == client_path.as_str())
            .unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                if site.file != client_file.id
                    || site.syntax_kind != "vba.invoked_get_member.reference"
                {
                    return false;
                }
                let span = site.source.span();
                let start = usize::try_from(span.start_byte()).unwrap();
                let end = usize::try_from(span.end_byte()).unwrap();
                client.as_bytes().get(start..end) == Some(b"Utility.Compute".as_slice())
            })
            .unwrap();
        let utility_path = derived_path(
            if foreign { "other.xlsm" } else { "book.xlsm" },
            "Utility",
            ModuleKind::Procedural,
        )
        .unwrap();
        let utility_file = document
            .files
            .iter()
            .find(|file| file.path == utility_path.as_str())
            .unwrap();
        let module = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Module
                    && entity.canonical_name == "Utility"
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == utility_file.id)
            })
            .unwrap();
        let qualifier = document
            .occurrences
            .iter()
            .find(|site| {
                if site.file != client_file.id
                    || site.syntax_kind != "vba.value_identifier.reference"
                {
                    return false;
                }
                let span = site.source.span();
                let start = usize::try_from(span.start_byte()).unwrap();
                let end = usize::try_from(span.end_byte()).unwrap();
                client.as_bytes().get(start..end) == Some(b"Utility".as_slice())
            })
            .unwrap();
        assert_eq!(
            matches!(qualifier.target, OccurrenceTarget::Resolved { symbol } if symbol == module.id),
            local.is_empty() && !foreign,
            "{visibility} {local} {foreign}"
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(qualifier.id)
                        && edge.predicate == RelationPredicate::RefersTo
                        && edge.object == RelationEndpoint::Entity(module.id)
                })
                .count(),
            usize::from(local.is_empty() && !foreign),
            "{visibility} {local} {foreign}"
        );
        assert_eq!(
            document.skipped_regions.iter().any(|gap| {
                gap.source == qualifier.source && gap.detail == "vba-binding-target-unavailable"
            }),
            !matches!(qualifier.target, OccurrenceTarget::Resolved { .. }),
            "source binding gap disagrees with its target: {visibility} {local} {foreign}"
        );
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            expected,
            "{visibility} {local} {foreign}"
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(site.id)
                        && edge.predicate == RelationPredicate::Calls
                })
                .count(),
            usize::from(expected),
            "{visibility} {local} {foreign}"
        );
    }
}

#[test]
fn native_project_enum_qualification_requires_public_unshadowed_members() {
    for (visibility, local, foreign, duplicate, expected) in [
        ("Public", "", false, false, true),
        ("Private", "", false, false, false),
        ("Public", "Dim Phase As Variant\r\n", false, false, false),
        ("Public", "", true, false, false),
        ("Public", "", false, true, false),
    ] {
        let root = tempdir();
        let client = format!(
            "Attribute VB_Name = \"Client\"\r\nPublic Sub Run()\r\n{local}Dim result As Long\r\nresult = Phase.Ready\r\nEnd Sub\r\n"
        );
        let globals = format!(
            "Attribute VB_Name = \"Globals\"\r\n{visibility} Enum Phase\r\nReady = 1\r\nEnd Enum\r\n"
        );
        if foreign {
            fs::write(
                root.path().join("book.xlsm"),
                fixture::book_with_project_value(
                    &[("Client", client.as_bytes(), false)],
                    "\"Project\"",
                ),
            )
            .unwrap();
            fs::write(
                root.path().join("other.xlsm"),
                fixture::book_with_project_value(
                    &[("Globals", globals.as_bytes(), false)],
                    "\"Project\"",
                ),
            )
            .unwrap();
        } else if duplicate {
            let other =
                b"Attribute VB_Name = \"Other\"\r\nPublic Enum Phase\r\nReady = 2\r\nEnd Enum\r\n";
            fs::write(
                root.path().join("book.xlsm"),
                fixture::book_with_project_value(
                    &[
                        ("Client", client.as_bytes(), false),
                        ("Globals", globals.as_bytes(), false),
                        ("Other", other, false),
                    ],
                    "\"Project\"",
                ),
            )
            .unwrap();
        } else {
            fs::write(
                root.path().join("book.xlsm"),
                fixture::book_with_project_value(
                    &[
                        ("Client", client.as_bytes(), false),
                        ("Globals", globals.as_bytes(), false),
                    ],
                    "\"Project\"",
                ),
            )
            .unwrap();
        }
        let mut service = FirstSliceService::new(4).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let client_path = derived_path("book.xlsm", "Client", ModuleKind::Procedural).unwrap();
        let client_file = document
            .files
            .iter()
            .find(|file| file.path == client_path.as_str())
            .unwrap();
        let globals_path = derived_path(
            if foreign { "other.xlsm" } else { "book.xlsm" },
            "Globals",
            ModuleKind::Procedural,
        )
        .unwrap();
        let globals_file = document
            .files
            .iter()
            .find(|file| file.path == globals_path.as_str())
            .unwrap();
        let enumeration = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Enum
                    && entity.canonical_name == "Phase"
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == globals_file.id)
            })
            .unwrap();
        let member = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Constant
                    && entity.canonical_name == "Ready"
                    && entity.container == Some(ContainerRef::Entity(enumeration.id))
            })
            .unwrap();
        for (syntax_kind, text, target) in [
            ("vba.get_member.reference", "Phase.Ready", member.id),
            ("vba.value_identifier.reference", "Phase", enumeration.id),
        ] {
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    if site.file != client_file.id || site.syntax_kind != syntax_kind {
                        return false;
                    }
                    let span = site.source.span();
                    let start = usize::try_from(span.start_byte()).unwrap();
                    let end = usize::try_from(span.end_byte()).unwrap();
                    client.as_bytes().get(start..end) == Some(text.as_bytes())
                })
                .unwrap();
            assert_eq!(
                matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == target),
                expected,
                "{visibility} {local} {foreign} {duplicate} {syntax_kind}: {:?}, enum {:?}, member {:?}",
                site.target,
                enumeration.visibility,
                member.visibility
            );
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|edge| {
                        edge.subject == RelationEndpoint::Occurrence(site.id)
                            && edge.predicate == RelationPredicate::RefersTo
                            && edge.object == RelationEndpoint::Entity(target)
                            && edge.evidence.source.as_ref() == Some(&site.source)
                    })
                    .count(),
                usize::from(expected),
                "{visibility} {local} {foreign} {duplicate} {syntax_kind}"
            );
        }
    }
}

#[test]
fn native_project_identity_issues_preserve_sources_and_follow_metadata_after_restart() {
    let storage = tempdir();
    let root = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    fs::write(
        root.path().join("fixed.xlsm"),
        fixture::book(&[("Alpha", FIRST)]),
    )
    .unwrap();
    let mut fixed_symbol = None;
    let mut previous_generation = None;
    for (value, issue) in [
        ("\"Project\"", None),
        ("\"Different\"", Some("Mismatch")),
        ("Project", Some("Quoting")),
        ("\"Project\"", None),
    ] {
        fs::write(
            root.path().join("book.xlsm"),
            fixture::book_with_project_value(&[("Alpha", FIRST, false)], value),
        )
        .unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(receipt.extracted_files, 2);
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        if let Some(previous) = previous_generation {
            assert_ne!(receipt.generation, previous);
        }
        previous_generation = Some(receipt.generation);
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let diagnostics = document
            .diagnostics
            .iter()
            .filter(|d| d.message.starts_with("vba project identity:"))
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.len(), usize::from(issue.is_some()));
        if let Some(issue) = issue {
            let diagnostic = diagnostics[0];
            assert_eq!(diagnostic.message, format!("vba project identity: {issue}"));
            assert_eq!(diagnostic.coverage_effect, CoverageStatus::Unknown);
            let file = diagnostic.source.as_ref().unwrap().span().file();
            assert_eq!(
                document.files.iter().find(|f| f.id == file).unwrap().path,
                "book.xlsm"
            );
        }
        for container in ["book.xlsm", "fixed.xlsm"] {
            let path = derived_path(container, "Alpha", ModuleKind::Procedural).unwrap();
            let reference = read_module(&service, receipt.generation, path.as_str(), FIRST);
            let symbol = document
                .entities
                .iter()
                .find(|e| {
                    e.canonical_name == "Compute"
                        && e.evidence.source.as_ref().unwrap().span().file()
                            == reference.span().file()
                })
                .unwrap()
                .id;
            if container == "fixed.xlsm" {
                if let Some(previous) = fixed_symbol {
                    assert_eq!(symbol, previous);
                }
                fixed_symbol = Some(symbol);
            }
        }
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
        assert_eq!(rebuilt.logical_snapshot, receipt.logical_snapshot);
        drop(service);
        let restored = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let restored_snapshot = restored
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        assert_eq!(
            restored_snapshot.document().diagnostics,
            document.diagnostics
        );
        let path = derived_path("book.xlsm", "Alpha", ModuleKind::Procedural).unwrap();
        read_module(&restored, receipt.generation, path.as_str(), FIRST);
    }
}

#[test]
fn native_office_classes_keep_exact_sources_members_and_restart_identity() {
    let storage = tempdir();
    let root = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let body = "Attribute VB_Name = \"Counter\"\r\nAttribute VB_GlobalNameSpace = False\r\nAttribute VB_Creatable = False\r\nAttribute VB_PredeclaredId = False\r\nAttribute VB_Exposed = False\r\nPrivate total As Long\r\nPublic Sub Refresh()\r\ntotal = 1\r\nEnd Sub\r\nPublic Sub Run()\r\nMe.Refresh\r\nEnd Sub\r\n";
    let path = derived_path("book.xlsm", "Counter", ModuleKind::Object).unwrap();
    let mut previous = None;
    for source in [body.to_owned(), body.replace("total = 1", "total = 2 + 3")] {
        fs::write(
            root.path().join("book.xlsm"),
            fixture::typed_book(&[
                ("Counter", source.as_bytes(), true),
                ("Beta", SECOND, false),
            ]),
        )
        .unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(receipt.extracted_files, 2);
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        read_module(
            &service,
            receipt.generation,
            path.as_str(),
            source.as_bytes(),
        );
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|f| f.path == path.as_str())
            .unwrap();
        let owner = document
            .entities
            .iter()
            .find(|e| e.canonical_name == "Counter")
            .unwrap();
        assert_eq!(owner.kind, EntityKind::Class);
        let identities = document
            .entities
            .iter()
            .filter(|e| {
                e.evidence
                    .source
                    .as_ref()
                    .is_some_and(|s| s.span().file() == file.id)
            })
            .map(|e| (e.canonical_name.clone(), e.id))
            .collect::<BTreeMap<_, _>>();
        if let Some(prior) = &previous {
            assert_eq!(&identities, prior);
        }
        previous = Some(identities.clone());
        for (name, kind) in [
            ("total", EntityKind::Field),
            ("Refresh", EntityKind::Method),
            ("Run", EntityKind::Method),
        ] {
            let member = document
                .entities
                .iter()
                .find(|e| e.canonical_name == name)
                .unwrap();
            assert_eq!(member.kind, kind);
            assert_eq!(
                member.container,
                Some(rootlight_ir::ContainerRef::Entity(owner.id))
            );
        }
        assert!(
            document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: identities["Refresh"]
                        })
        );
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
        assert_eq!(rebuilt.logical_snapshot, receipt.logical_snapshot);
        drop(service);
        let restored = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        read_module(
            &restored,
            receipt.generation,
            path.as_str(),
            source.as_bytes(),
        );
        let restored = restored
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        assert!(
            restored
                .document()
                .entities
                .iter()
                .any(|e| e.id == owner.id && e.kind == EntityKind::Class)
        );
    }
}

#[test]
fn native_office_metadata_changes_do_not_reuse_foreign_module_kinds() {
    let root = tempdir();
    let source = b"Attribute VB_Name = \"Counter\"\r\nPublic Sub Refresh()\r\nEnd Sub\r\nPublic Sub Run()\r\nMe.Refresh\r\nEnd Sub\r\n";
    fs::write(
        root.path().join("fixed.xlsm"),
        fixture::typed_book(&[("Counter", source, true)]),
    )
    .unwrap();
    let mut service = FirstSliceService::new(8).unwrap();
    let mut fixed_identity = None;
    let mut prior_generation = None;
    for object in [false, true, false] {
        fs::write(
            root.path().join("changing.xlsm"),
            fixture::typed_book(&[("Counter", source, object)]),
        )
        .unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(receipt.extracted_files, 2);
        assert_ne!(prior_generation, Some(receipt.generation));
        prior_generation = Some(receipt.generation);
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let owners: Vec<_> = document
            .entities
            .iter()
            .filter(|e| e.canonical_name == "Counter")
            .collect();
        assert_eq!(owners.len(), 2);
        assert_ne!(owners[0].id, owners[1].id);
        for (container, is_object) in [("changing.xlsm", object), ("fixed.xlsm", true)] {
            let kind = if is_object {
                ModuleKind::Object
            } else {
                ModuleKind::Procedural
            };
            let path = derived_path(container, "Counter", kind).unwrap();
            let reference = read_module(&service, receipt.generation, path.as_str(), source);
            let owner = owners
                .iter()
                .find(|e| {
                    e.evidence.source.as_ref().unwrap().span().file() == reference.span().file()
                })
                .unwrap();
            assert_eq!(
                owner.kind,
                if is_object {
                    EntityKind::Class
                } else {
                    EntityKind::Module
                }
            );
            if container == "fixed.xlsm" {
                if let Some(previous) = fixed_identity {
                    assert_eq!(owner.id, previous);
                }
                fixed_identity = Some(owner.id);
            }
            let refresh = document
                .entities
                .iter()
                .find(|e| {
                    e.canonical_name == "Refresh"
                        && e.container == Some(rootlight_ir::ContainerRef::Entity(owner.id))
                })
                .unwrap();
            assert_eq!(
                refresh.kind,
                if is_object {
                    EntityKind::Method
                } else {
                    EntityKind::Function
                }
            );
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.self_invoked_member.reference"
                        && site.source.span().file() == reference.span().file()
                })
                .unwrap();
            if is_object {
                assert_eq!(
                    site.target,
                    OccurrenceTarget::Resolved { symbol: refresh.id }
                );
            } else {
                assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            }
        }
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
        assert_eq!(rebuilt.logical_snapshot, receipt.logical_snapshot);
    }
}

#[test]
fn derived_modules_survive_edit_noop_rebuild_removal_and_restart() {
    let storage = tempdir();
    let root = tempdir();
    let cancellation = deadline();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let book = root.path().join("book.xlsm");
    let original = fixture::book(&[("Alpha", FIRST), ("Beta", SECOND)]);
    fs::write(&book, &original).unwrap();
    let mut service = FirstSliceService::new_durable(5, paths.state_dir(), &cancellation).unwrap();
    let first = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(
        first.discovered_inputs, 1,
        "physical discovery excludes virtual modules"
    );
    assert_eq!(
        first.indexed_files, 3,
        "original container plus two real derived sources"
    );
    assert_eq!(serde_json::to_value(&first).unwrap()["extracted_files"], 2);
    let alpha = derived_path("book.xlsm", "Alpha", ModuleKind::Procedural).unwrap();
    let beta = derived_path("book.xlsm", "Beta", ModuleKind::Procedural).unwrap();
    let stale = read_module(&service, first.generation, alpha.as_str(), FIRST);
    read_module(&service, first.generation, beta.as_str(), SECOND);
    let snapshot = service
        .loaded_generation_snapshot(first.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.version, NormalizedIrVersion::V1_6);
    assert_eq!(
        document
            .source_mappings
            .iter()
            .filter(|mapping| mapping.kind == SourceMappingKind::ExtractedFromContainer)
            .count(),
        2
    );
    assert!(
        !document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "container-source-incomplete")
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.display_name == "Compute")
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.display_name == "Dispatch")
    );
    assert_eq!(
        service
            .index_repository(root.path(), &cancellation)
            .unwrap(),
        first
    );
    let changed = String::from_utf8(FIRST.to_vec())
        .unwrap()
        .replace("= 1", "= 2")
        .into_bytes();
    fs::write(
        &book,
        fixture::book(&[("Beta", SECOND), ("Alpha", &changed)]),
    )
    .unwrap();
    let updated = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(updated.extracted_files, 2);
    let current = read_module(&service, updated.generation, alpha.as_str(), &changed);
    assert_eq!(
        stale.span().file(),
        current.span().file(),
        "stream reorder does not change source identity"
    );
    assert_ne!(first.generation, updated.generation);
    assert!(
        service
            .source_read(updated.generation, vec![stale], &cancellation)
            .is_err()
    );
    assert_eq!(
        service
            .incremental_evidence(updated.generation)
            .unwrap()
            .parsed_files(),
        1
    );
    let prepared = service
        .prepare_repository_with_options(
            root.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
            &cancellation,
        )
        .unwrap();
    let clean = service.publish_prepared(prepared, &cancellation).unwrap();
    assert_eq!(updated.logical_snapshot, clean.logical_snapshot);
    assert_eq!(clean.extracted_files, 2);
    fs::write(&book, fixture::book(&[("Alpha", &changed)])).unwrap();
    let removed = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(removed.indexed_files, 2);
    assert_eq!(removed.extracted_files, 1);
    assert!(
        !service
            .loaded_generation_snapshot(removed.generation)
            .unwrap()
            .document()
            .files
            .iter()
            .any(|file| file.path == beta.as_str())
    );
    drop(service);
    fs::remove_file(&book).unwrap();
    let restored = FirstSliceService::new_durable(5, paths.state_dir(), &cancellation).unwrap();
    read_module(&restored, first.generation, alpha.as_str(), FIRST);
    read_module(&restored, updated.generation, alpha.as_str(), &changed);
    read_module(&restored, removed.generation, alpha.as_str(), &changed);
    for receipt in [&first, &updated, &removed] {
        let recovered = restored.receipts.get(&receipt.generation).unwrap();
        assert_eq!(recovered.extracted_files, receipt.extracted_files);
        assert_eq!(recovered.indexed_files - recovered.extracted_files, 1);
    }
}

#[test]
fn unverified_module_keeps_source_and_does_not_hide_verified_sibling() {
    let root = tempdir();
    let wrong = b"Attribute VB_Name = \"Wrong\"\r\nPublic Sub Hidden()\r\nEnd Sub\r\n";
    fs::write(
        root.path().join("book.xlsm"),
        fixture::book(&[("Alpha", FIRST), ("Beta", wrong)]),
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(receipt.indexed_files, 3);
    assert_eq!(receipt.extracted_files, 2);
    read_module(
        &service,
        receipt.generation,
        derived_path("book.xlsm", "Beta", ModuleKind::Procedural)
            .unwrap()
            .as_str(),
        wrong,
    );
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    assert!(
        snapshot
            .document()
            .entities
            .iter()
            .any(|entity| entity.display_name == "Compute")
    );
    assert!(
        !snapshot
            .document()
            .entities
            .iter()
            .any(|entity| entity.display_name == "Hidden")
    );
    assert!(
        snapshot
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "container-source-incomplete")
    );
}

#[test]
fn derived_paths_escape_stream_names_and_preserve_exact_unicode_identity() {
    let names = ["../Alpha", "a/b", "a\\b", "CON", "α", "é", "e\u{301}"];
    let paths = names
        .iter()
        .map(|name| {
            derived_path("nested/book.xlsm", name, ModuleKind::Procedural)
                .unwrap()
                .as_str()
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(paths.len(), names.len());
    assert!(
        paths
            .iter()
            .all(|path| path.starts_with("nested/book.xlsm/@vba/") && path.ends_with(".bas"))
    );
}

#[test]
fn admission_failure_spends_reservation_and_never_wraps() {
    let mut budget = Budget {
        work: 64,
        bytes: 64,
        files: 2,
    };
    let reservation = budget.reserve();
    assert_eq!(budget.work, 0);
    assert!(budget.settle(reservation, 65).is_err());
    budget.settle(reservation, 16).unwrap();
    assert_eq!(budget.work, 48);
}

fn prepared_fixture(
    bytes: &[u8],
    files: usize,
    capacity: usize,
    cancellation: &Cancellation,
) -> (PreparedSources, BTreeMap<FileId, SourceSnapshot>) {
    let repository = RepositoryId::from_bytes([81; 16]);
    let path = RelativePath::parse(Path::new("book.xlsm")).unwrap();
    let file = derive_file(rootlight_ids::FileIdentity {
        repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    let hash = content_hash(bytes);
    let snapshot =
        SourceSnapshot::from_persisted(repository, path, file, hash, bytes.to_vec()).unwrap();
    let input = ManifestInput {
        file,
        path: "book.xlsm".to_owned(),
        content_hash: hash,
        bytes: u64::try_from(bytes.len()).unwrap(),
        encoding: "binary".to_owned(),
        class: InputClass::Source,
        language_signals: vec![LanguageSignal {
            language: "office-open-xml".to_owned(),
            evidence: LanguageEvidence::Content,
        }],
        decisive_rule: None,
    };
    let mut snapshots = BTreeMap::from([(file, snapshot)]);
    let prepared = prepare(
        &[input],
        &mut snapshots,
        repository,
        files,
        capacity,
        cancellation,
    )
    .unwrap();
    (prepared, snapshots)
}

#[test]
fn native_predeclared_inventory_retains_only_verified_effective_evidence() {
    for (attributes, valid, expected) in [
        (
            "Attribute VB_PredeclaredId = True\r\n",
            true,
            Some(b"True".as_slice()),
        ),
        (
            "Attribute VB_PredeclaredId = True\r\nAttribute VB_PredeclaredId = False\r\n",
            true,
            None,
        ),
        ("Attribute VB_PredeclaredId = \"True\"\r\n", false, None),
        ("", true, None),
    ] {
        let source = format!(
            "Attribute VB_Name = \"Object\"\r\n{attributes}Public Sub Run()\r\nEnd Sub\r\n"
        );
        let bytes = fixture::native_book(
            &[("Object", source.as_bytes(), ProjectModuleKind::Class)],
            "\"Project\"",
        );
        let (prepared, snapshots) = prepared_fixture(&bytes, 1, 1_000_000, &deadline());
        let project = &prepared.native_projects[0];
        assert_eq!(project.inventory_complete, valid);
        let member = &project.members[0];
        let retained = snapshots[&member.file.unwrap()].content();
        assert_eq!(retained, source.as_bytes());
        assert_eq!(
            member.predeclared_id.clone().map(|range| &retained[range]),
            expected
        );
        assert_eq!(
            prepared
                .native_vba_modules
                .contains_key(&member.file.unwrap()),
            valid
        );
        assert_eq!(prepared.issues.is_empty(), valid);
    }
}

#[test]
fn native_project_inventory_keeps_unadmitted_and_unverified_members() {
    let bytes = fixture::book(&[("Alpha", FIRST), ("Beta", SECOND)]);
    for (files, admitted) in [(1, 1), (2, 2)] {
        let (prepared, snapshots) = prepared_fixture(&bytes, files, 1_000_000, &deadline());
        assert_eq!(prepared.native_projects.len(), 1);
        let project = &prepared.native_projects[0];
        assert_eq!(project.content_hash, content_hash(&bytes));
        assert_eq!(snapshots[&project.container].content(), bytes);
        assert_eq!(project.name.as_deref(), Some("Project"));
        assert_eq!(project.members.len(), 2);
        assert_eq!(
            project.members.iter().filter(|m| m.file.is_some()).count(),
            admitted
        );
        assert_eq!(project.members[0].name.as_deref(), Some("Alpha"));
        assert_eq!(project.members[1].name.as_deref(), Some("Beta"));
    }
    let bytes = fixture::book(&[("Alpha", FIRST), ("Beta", FIRST)]);
    let (prepared, _) = prepared_fixture(&bytes, 2, 1_000_000, &deadline());
    let project = &prepared.native_projects[0];
    assert!(!project.inventory_complete);
    assert_eq!(project.members.len(), 2);
    assert!(project.members.iter().all(|m| m.file.is_some()));
    assert_eq!(
        prepared
            .inputs
            .iter()
            .filter(|i| i
                .language_signals
                .iter()
                .any(|s| s.language == "vba-unverified"))
            .count(),
        1
    );
}

#[test]
fn native_project_resolution_checks_identity_and_charges_preparation_work() {
    use rootlight_resolve::{
        ClassicProjectSource, NativeClassicMember, NativeClassicMemberKind, NativeClassicProject,
        ResolutionError,
    };
    let root = tempdir();
    let bytes = fixture::book(&[("Alpha", FIRST), ("Beta", SECOND)]);
    fs::write(root.path().join("book.xlsm"), &bytes).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let container = document
        .files
        .iter()
        .find(|f| f.path == "book.xlsm")
        .unwrap();
    let mut project = NativeClassicProject {
        container: container.id,
        content_hash: container.content_hash,
        name: Some("Project".to_owned()),
        members: Vec::new(),
        inventory_complete: true,
    };
    let mut inputs = Vec::new();
    for (name, body) in [("Alpha", FIRST), ("Beta", SECOND)] {
        let path = derived_path("book.xlsm", name, ModuleKind::Procedural).unwrap();
        let file = document
            .files
            .iter()
            .find(|f| f.path == path.as_str())
            .unwrap();
        project.members.push(NativeClassicMember {
            file: Some(file.id),
            name: Some(name.to_owned()),
            kind: Some(NativeClassicMemberKind::Procedural),
            predeclared_id: None,
        });
        inputs.push(ClassicProjectSource::new(file.id, body));
    }
    let context = ResolverFactContext::new(content_hash(RESOLVER_BINARY_SEED));
    let engine = ResolutionEngine::default();
    let (complete, estimate) = engine
        .apply_document_bounded_with_native_projects(
            document.clone(),
            context,
            &inputs,
            std::slice::from_ref(&project),
            &deadline(),
        )
        .unwrap();
    assert!(estimate.fits());
    assert_eq!(&complete, document);
    let limits = ResolutionLimits::with_work_limit(8, 1).unwrap();
    let (bounded, estimate) = ResolutionEngine::new(limits)
        .apply_document_bounded_with_native_projects(
            document.clone(),
            context,
            &inputs,
            std::slice::from_ref(&project),
            &deadline(),
        )
        .unwrap();
    assert!(!estimate.fits());
    assert_eq!(&bounded, document);
    let warned = apply_bounded_resolution_with_native_projects(
        document.clone(),
        limits,
        context,
        &inputs,
        std::slice::from_ref(&project),
        &deadline(),
    )
    .unwrap();
    assert!(
        warned
            .diagnostics
            .iter()
            .any(|d| d.code == "resolution-work-bounded")
    );
    assert!(matches!(
        engine.apply_document_bounded_with_native_projects(
            document.clone(),
            context,
            &inputs,
            &[project.clone(), project.clone()],
            &deadline()
        ),
        Err(ResolutionError::ClassicSourceIdentity)
    ));
    project.content_hash = content_hash(b"stale container");
    assert!(matches!(
        engine.apply_document_bounded_with_native_projects(
            document.clone(),
            context,
            &inputs,
            std::slice::from_ref(&project),
            &deadline()
        ),
        Err(ResolutionError::ClassicSourceIdentity)
    ));
    project.content_hash = container.content_hash;
    for range in [0..4, 0..usize::MAX] {
        project.members[0].predeclared_id = Some(range);
        assert!(matches!(
            engine.apply_document_bounded_with_native_projects(
                document.clone(),
                context,
                &inputs,
                std::slice::from_ref(&project),
                &deadline()
            ),
            Err(ResolutionError::ClassicSourceIdentity)
        ));
    }
    project.members[0].predeclared_id = None;
    inputs[0] = ClassicProjectSource::new(project.members[0].file.unwrap(), b"stale source");
    assert!(matches!(
        engine.apply_document_bounded_with_native_projects(
            document.clone(),
            context,
            &inputs,
            &[project],
            &deadline()
        ),
        Err(ResolutionError::ClassicSourceIdentity)
    ));
}

#[test]
fn aggregate_admission_preserves_container_and_reports_remaining_modules() {
    let bytes = fixture::book(&[("Alpha", FIRST), ("Beta", SECOND)]);
    let (prepared, snapshots) = prepared_fixture(&bytes, 1, 1_000_000, &deadline());
    assert_eq!(prepared.inputs.len(), 1);
    assert_eq!(snapshots.len(), 2);
    assert!(
        prepared
            .issues
            .iter()
            .any(|(_, message)| message.contains("remaining declarations: 1"))
    );
    let (prepared, snapshots) = prepared_fixture(&bytes, 2, 16, &deadline());
    assert!(prepared.inputs.is_empty());
    assert_eq!(snapshots.len(), 1);
    assert!(!prepared.issues.is_empty());
}

#[test]
fn declared_codepage_produces_exact_utf8_without_changing_original() {
    let source =
        b"Attribute VB_Name = \"Alpha\"\r\n' caf\xe9\r\nPublic Sub Compute()\r\nEnd Sub\r\n";
    let original = fixture::book(&[("Alpha", source)]);
    let (prepared, snapshots) = prepared_fixture(&original, 2, 1_000_000, &deadline());
    assert!(prepared.issues.is_empty(), "{:?}", prepared.issues);
    assert_eq!(prepared.inputs.len(), 1);
    let (derived, container) = prepared.origins[0];
    assert_eq!(snapshots[&container].content(), original);
    assert_eq!(
        snapshots[&derived].content(),
        "Attribute VB_Name = \"Alpha\"\r\n' café\r\nPublic Sub Compute()\r\nEnd Sub\r\n".as_bytes()
    );
    assert_eq!(prepared.fingerprints.len(), 2);
}

#[test]
fn identical_module_names_in_distinct_containers_remain_distinct() {
    let root = tempdir();
    let bytes = fixture::book(&[("Alpha", FIRST)]);
    fs::write(root.path().join("first.xlsm"), &bytes).unwrap();
    fs::write(root.path().join("second.xlsm"), &bytes).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(receipt.discovered_inputs, 2);
    assert_eq!(receipt.indexed_files, 4);
    assert_eq!(receipt.extracted_files, 2);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let symbols = snapshot
        .document()
        .entities
        .iter()
        .filter(|entity| entity.display_name == "Compute")
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(symbols.len(), 2);
    assert_eq!(
        snapshot
            .document()
            .source_mappings
            .iter()
            .filter(|mapping| mapping.kind == SourceMappingKind::ExtractedFromContainer)
            .count(),
        2
    );
}

#[test]
fn extracted_counts_restore_legacy_manifests_and_reject_false_current_counts() {
    let storage = tempdir();
    let root = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let book = root.path().join("book.xlsm");
    fs::write(&book, fixture::book(&[("Alpha", FIRST), ("Beta", SECOND)])).unwrap();
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &deadline()).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let manifest_path = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string())
        .join("manifest.json");
    let original = fs::read(&manifest_path).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(manifest["version"], 4);
    assert_eq!(manifest["receipt"]["extracted_files"], 2);
    let retained_without_manifest = receipt
        .retained_durable_bytes
        .checked_sub(u64::try_from(original.len()).unwrap())
        .unwrap();
    fs::remove_file(&book).unwrap();
    for (version, extracted, discovered, valid) in [
        (4, Some(2), 1, true),
        (4, None, 1, false),
        (4, Some(0), 1, false),
        (4, Some(3), 1, false),
        (4, Some(u64::MAX), 1, false),
        (4, Some(2), 0, false),
        (3, None, 1, true),
        (3, None, 0, false),
    ] {
        let mut changed = manifest.clone();
        changed["version"] = version.into();
        changed["receipt"]["discovered_inputs"] = discovered.into();
        match extracted {
            Some(count) => changed["receipt"]["extracted_files"] = count.into(),
            None => {
                changed["receipt"]
                    .as_object_mut()
                    .unwrap()
                    .remove("extracted_files");
            }
        }
        let bytes = loop {
            let bytes = serde_json::to_vec(&changed).unwrap();
            let retained = retained_without_manifest
                .checked_add(u64::try_from(bytes.len()).unwrap())
                .unwrap();
            if changed["receipt"]["retained_durable_bytes"] == retained {
                break bytes;
            }
            changed["receipt"]["retained_durable_bytes"] = retained.into();
        };
        fs::write(&manifest_path, bytes).unwrap();
        let result = service.durable.as_ref().unwrap().restore_exact_generation(
            receipt.repository,
            receipt.generation,
            &deadline(),
        );
        if valid {
            let restored = result.unwrap();
            assert_eq!(restored.receipt.discovered_inputs, 1);
            assert_eq!(restored.receipt.indexed_files, 3);
            assert_eq!(restored.receipt.extracted_files, 2);
        } else {
            assert!(matches!(result, Err(FirstSliceError::CatalogCorrupt)));
        }
    }
}
