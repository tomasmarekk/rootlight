//! Lexical Text and exact literal retrieval retain distinct public contracts.
//!
//! Internal fragments need literal source evidence; durable reopen must preserve
//! both query classifications and physical bytes after live sources change.

use super::*;
use rootlight_query::ExecutionCompleteness;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop, SourceReadOptions};
use rootlight_vfs::TextEncoding;

const SOURCE: &str =
    "-- boxedpair λ😀\r\nclass BOX_PAIR\r\nfeature\r\n    count: INTEGER\r\nend\r\n";

fn encode(text: &str, encoding: TextEncoding, bom: bool) -> Vec<u8> {
    match encoding {
        TextEncoding::Utf8 => {
            let mut bytes = if bom {
                vec![0xef, 0xbb, 0xbf]
            } else {
                Vec::new()
            };
            bytes.extend_from_slice(text.as_bytes());
            bytes
        }
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            let mut bytes = if bom {
                match encoding {
                    TextEncoding::Utf16Le => vec![0xff, 0xfe],
                    TextEncoding::Utf16Be => vec![0xfe, 0xff],
                    TextEncoding::Utf8 => unreachable!(),
                }
            } else {
                Vec::new()
            };
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&match encoding {
                    TextEncoding::Utf16Le => unit.to_le_bytes(),
                    TextEncoding::Utf16Be => unit.to_be_bytes(),
                    TextEncoding::Utf8 => unreachable!(),
                });
            }
            bytes
        }
    }
}

fn assert_public_queries(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    sources: &[(String, TextEncoding, Vec<u8>)],
) {
    let cancellation = Cancellation::new();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    for (path, encoding, original) in sources {
        let file = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == *path)
            .unwrap();
        assert_eq!(file.language, "eiffel");
        assert_eq!(file.encoding, encoding.as_str());
        assert_eq!(file.byte_length, u64::try_from(original.len()).unwrap());
        assert_eq!(file.content_hash, content_hash(original));
        for (query, matched) in [
            ("pair", true),
            ("BOX_P", true),
            ("library/comp", true),
            ("pai", false),
        ] {
            let located = service
                .code_locate_with_filters_and_budget(
                    receipt.generation,
                    query.to_owned(),
                    LocateMode::Text,
                    Vec::new(),
                    vec![path.clone()],
                    64,
                    0,
                    FirstSliceBudget::default(),
                    &cancellation,
                )
                .unwrap();
            assert_eq!(located.data.generation, receipt.generation);
            assert_eq!(located.data.execution, ExecutionCompleteness::complete());
            assert!(!located.data.truncated);
            assert!(located.data.limiting_resources.is_empty());
            assert!(located.data.next_page_offset.is_none());
            assert_eq!(
                located.data.matched_candidates,
                u64::try_from(located.data.hits.len()).unwrap()
            );
            assert_eq!(
                !located.data.hits.is_empty(),
                matched,
                "query={query}, path={path}"
            );
            for hit in &located.data.hits {
                assert_eq!(hit.file, file.id);
                assert_eq!(hit.path, *path);
                assert_eq!(hit.language, "eiffel");
                let source = hit
                    .source
                    .as_ref()
                    .expect("located source evidence is present");
                assert_eq!(source.repository(), receipt.repository);
                assert_eq!(source.generation(), receipt.generation);
                assert_eq!(source.span().file(), file.id);
                assert_eq!(source.content_hash(), file.content_hash);
            }
            if query == "pair" {
                assert!(
                    located
                        .data
                        .hits
                        .iter()
                        .any(|hit| hit.symbol.is_some() && hit.identifier == "BOX_PAIR")
                );
            }
        }

        let whole = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
            file.content_hash,
            None,
        );
        let options = SourceReadOptions::new()
            .with_context_lines_before(0)
            .with_context_lines_after(0)
            .with_encoding(SourceEncoding::Bytes);
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![whole.clone()],
                options,
                FirstSliceBudget::default(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, whole);
        assert_eq!(&read.data.chunks[0].bytes, original);

        for literal in ["pai", "PAI", "Pai"] {
            let physical_literal = encode(literal, *encoding, false);
            let expected = original
                .windows(physical_literal.len())
                .enumerate()
                .filter(|(_, bytes)| *bytes == physical_literal)
                .map(|(offset, _)| {
                    (
                        u64::try_from(offset).unwrap(),
                        u64::try_from(offset + physical_literal.len()).unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(expected.len(), usize::from(literal != "Pai"));
            let mut budget = SourceFindBudget::new();
            budget.matcher.max_scan_bytes = 7;
            budget.matcher.max_matches = 1;
            let mut cursor = None;
            // UTF-16 text omits its BOM; the full hash and raw read still cover it.
            let mut scanned = match encoding {
                TextEncoding::Utf8 => 0,
                TextEncoding::Utf16Le | TextEncoding::Utf16Be => 2,
            };
            let mut matches = Vec::new();
            let mut pages = 0;
            loop {
                let found = service
                    .find_literal(
                        receipt.generation,
                        file.id,
                        literal,
                        cursor,
                        budget,
                        &cancellation,
                    )
                    .unwrap();
                assert_eq!(found.generation, receipt.generation);
                assert_eq!(found.file.id, file.id);
                assert_eq!(found.file.path, *path);
                assert_eq!(found.file.language, "eiffel");
                assert_eq!(found.file.content_hash, file.content_hash);
                assert_eq!(found.file.byte_length, file.byte_length);
                assert_eq!(found.file.encoding, *encoding);
                assert_eq!(found.scanned.start, scanned);
                assert!(found.scanned.end > scanned);
                assert!(found.scanned.end - scanned <= 7);
                scanned = found.scanned.end;
                assert!(found.matches.len() <= 1);
                matches.extend(found.matches);
                cursor = found.continuation;
                pages += 1;
                assert!(pages <= original.len() + 1);
                if cursor.is_none() {
                    assert_eq!(found.stop, SourceFindStop::EndOfSource);
                    break;
                }
                assert_ne!(found.stop, SourceFindStop::EndOfSource);
            }
            assert_eq!(scanned, file.byte_length);
            assert_eq!(
                matches
                    .iter()
                    .map(|reference| (reference.span().start_byte(), reference.span().end_byte()))
                    .collect::<Vec<_>>(),
                expected
            );
            for reference in matches {
                assert_eq!(reference.repository(), receipt.repository);
                assert_eq!(reference.generation(), receipt.generation);
                assert_eq!(reference.span().file(), file.id);
                assert_eq!(reference.content_hash(), file.content_hash);
                let read = service
                    .source_read_with_options_and_budget(
                        receipt.generation,
                        vec![reference.clone()],
                        options,
                        FirstSliceBudget::default(),
                        &cancellation,
                    )
                    .unwrap();
                assert_eq!(read.data.chunks.len(), 1);
                assert_eq!(read.data.chunks[0].reference, reference);
                assert_eq!(read.data.chunks[0].bytes, physical_literal);
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn eiffel_text_and_literal_queries_preserve_distinct_contracts_after_reopen() {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    fs::create_dir(fixture.path().join("library")).unwrap();
    let sources = [
        (TextEncoding::Utf8, false),
        (TextEncoding::Utf8, true),
        (TextEncoding::Utf16Le, true),
        (TextEncoding::Utf16Be, true),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (encoding, bom))| {
        (
            format!("library/compound-{index}.e"),
            encoding,
            encode(SOURCE, encoding, bom),
        )
    })
    .collect::<Vec<_>>();
    for (path, _, bytes) in &sources {
        fs::write(fixture.path().join(path), bytes).unwrap();
    }
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(receipt.indexed_files, u64::try_from(sources.len()).unwrap());
    assert_public_queries(&service, &receipt, &sources);
    for (path, _, _) in &sources {
        fs::write(fixture.path().join(path), b"changed live source").unwrap();
    }
    drop(service);
    let reopened = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    assert_eq!(reopened.active_generation(), Some(receipt.generation));
    assert_public_queries(&reopened, &receipt, &sources);
}
