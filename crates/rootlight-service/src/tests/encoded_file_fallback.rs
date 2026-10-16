//! Resource-bounded analysis must preserve every safe source encoding.
//! Exercise the separate file catalog through publication, updates and reopen
//! so its records remain exact retrieval evidence rather than parser claims.

use super::*;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop, SourceReadOptions};

fn encode(source: &str, encoding: &str) -> Vec<u8> {
    match encoding {
        "utf-8" => source.as_bytes().to_vec(),
        "utf-8-bom" => [b"\xef\xbb\xbf".as_slice(), source.as_bytes()].concat(),
        "utf-16le" => std::iter::once(0xfeff)
            .chain(source.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect(),
        "utf-16be" => std::iter::once(0xfeff)
            .chain(source.encode_utf16())
            .flat_map(u16::to_be_bytes)
            .collect(),
        _ => unreachable!("the fixture has exactly four authored encodings"),
    }
}

fn assert_sources(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    sources: &[(String, &str, Vec<u8>)],
) {
    let cancellation = Cancellation::new();
    let generation = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    assert_eq!(generation.file_count(), sources.len());
    assert_eq!(generation.document().files.len(), 1);
    assert_eq!(generation.source_files().len(), sources.len() - 1);
    assert!(generation.document().entities.is_empty());
    assert!(generation.document().occurrences.is_empty());
    assert!(generation.document().relations.is_empty());
    for (path, encoding, original) in sources {
        let file = generation
            .document()
            .files
            .iter()
            .chain(
                generation
                    .source_files()
                    .entries()
                    .iter()
                    .map(|entry| entry.file()),
            )
            .find(|file| file.path == *path)
            .unwrap();
        assert_eq!(
            file.encoding,
            encoding.strip_suffix("-bom").unwrap_or(encoding)
        );
        assert_eq!(file.language, "rust");
        assert_eq!(file.byte_length, u64::try_from(original.len()).unwrap());
        assert_eq!(file.content_hash, content_hash(original));
        let located = service
            .code_locate(
                receipt.generation,
                path.clone(),
                LocateMode::Exact,
                10,
                0,
                &cancellation,
            )
            .unwrap();
        assert!(
            located
                .data
                .hits
                .iter()
                .any(|hit| hit.path == *path && hit.symbol.is_none())
        );
        let reference = file.evidence.source.clone().unwrap();
        assert_eq!(reference.span().start_byte(), 0);
        assert_eq!(reference.span().end_byte(), file.byte_length);
        let read = service
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
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, reference);
        assert_eq!(&read.data.chunks[0].bytes, original);
        let literal = "λ😀";
        let physical_literal = if encoding.starts_with("utf-16") {
            encode(literal, encoding)[2..].to_vec()
        } else {
            literal.as_bytes().to_vec()
        };
        let start = original
            .windows(physical_literal.len())
            .position(|window| window == physical_literal)
            .unwrap();
        let found = service
            .find_literal(
                receipt.generation,
                file.id,
                literal,
                None,
                SourceFindBudget::new(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(found.stop, SourceFindStop::EndOfSource);
        assert_eq!(found.matches.len(), 1);
        assert_eq!(
            found.matches[0].span().start_byte(),
            u64::try_from(start).unwrap()
        );
        assert_eq!(
            found.matches[0].span().end_byte(),
            u64::try_from(start + physical_literal.len()).unwrap()
        );
        assert_eq!(found.matches[0].content_hash(), file.content_hash);
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &cancellation)
            .unwrap();
        assert_eq!(coverage.language, "rust");
        assert_eq!(coverage.status, CoverageStatus::Unknown);
        assert_eq!(
            coverage.reason,
            Some(FirstSliceSourceCoverageReason::ResourceBounded)
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn file_only_fallback_preserves_encoded_sources_across_updates_and_reopen() {
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    let original = "// λ😀 encodedFallbackCanary\npub fn written_value() -> u8 { 1 }\n";
    let sources = ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"]
        .into_iter()
        .enumerate()
        .map(|(index, encoding)| {
            (
                format!("source-{index}.rs"),
                encoding,
                encode(original, encoding),
            )
        })
        .collect::<Vec<_>>();
    for (path, _, bytes) in &sources {
        fs::write(fixture.path().join(path), bytes).unwrap();
    }
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation).unwrap();
    let mut limits = service.analysis_limits.ir().clone();
    limits.max_files = 1;
    replace_service_ir_limits(&mut service, limits);
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .expect("all safe encodings publish a resource-bounded file catalog");
    assert_sources(&service, &receipt, &sources);
    assert_eq!(
        service
            .index_repository(fixture.path(), &cancellation)
            .unwrap(),
        receipt
    );
    let mut updated_sources = sources.clone();
    updated_sources[2].2 = encode(&original.replace("{ 1 }", "{ 2 }"), "utf-16le");
    fs::write(
        fixture.path().join(&updated_sources[2].0),
        &updated_sources[2].2,
    )
    .unwrap();
    let update = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_ne!(update.generation, receipt.generation);
    assert_eq!(update.parent, Some(receipt.generation));
    assert_sources(&service, &update, &updated_sources);
    assert_sources(&service, &receipt, &sources);
    assert_eq!(
        service
            .index_repository(fixture.path(), &cancellation)
            .unwrap(),
        update
    );
    for (path, _, _) in &sources {
        fs::write(
            fixture.path().join(path),
            b"live bytes replaced after publication",
        )
        .unwrap();
    }
    drop(service);
    let restored = FirstSliceService::new_durable(3, paths.state_dir(), &cancellation).unwrap();
    assert_sources(&restored, &update, &updated_sources);
    assert_sources(&restored, &receipt, &sources);
}
