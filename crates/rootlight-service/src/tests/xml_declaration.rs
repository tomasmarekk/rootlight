//! XML declaration admission preserves exact files without false native facts.
//! Valid siblings and durable updates remain usable when a complete source has
//! no verified interpretation under its declared encoding.

use super::*;

#[path = "../../examples/reopen_query/paged_source.rs"]
mod paged_source;

#[test]
fn xliff_routing_requires_content_and_preserves_configuration() {
    let service = FirstSliceService::new(1).unwrap();
    for path in ["messages.xlf", "MESSAGES.XLF", "messages.xml.xlf"] {
        let mut input = ManifestInput {
            file: FileId::from_bytes([9; 20]),
            path: path.to_owned(),
            content_hash: content_hash(b"<xliff/>"),
            bytes: 8,
            encoding: "utf-8".to_owned(),
            class: InputClass::Source,
            language_signals: vec![rootlight_discovery::LanguageSignal {
                language: "xml".to_owned(),
                evidence: LanguageEvidence::Extension,
            }],
            project_language_witnesses: Vec::new(),
            decisive_rule: None,
        };
        assert_eq!(source_language_route(&input, &service.analyzers), None);
        input.language_signals[0].evidence = LanguageEvidence::Content;
        assert_eq!(
            supported_source_language(&input, &service.analyzers),
            Some("xml")
        );
        let mut absent = service.analyzers.clone();
        assert!(absent.remove("xml").is_some());
        assert_eq!(source_language_route(&input, &absent), None);
        for evidence in [
            LanguageEvidence::Content,
            LanguageEvidence::Shebang,
            LanguageEvidence::Extension,
        ] {
            input
                .language_signals
                .push(rootlight_discovery::LanguageSignal {
                    language: "python".to_owned(),
                    evidence,
                });
            assert_eq!(source_language_route(&input, &service.analyzers), None);
            input.language_signals.pop();
        }
        input
            .language_signals
            .push(rootlight_discovery::LanguageSignal {
                language: "json".to_owned(),
                evidence: LanguageEvidence::Configuration,
            });
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            Some("json")
        );
        input.language_signals.last_mut().unwrap().language = "unavailable-xml-parser".to_owned();
        assert_eq!(source_language_route(&input, &service.analyzers), None);
    }
}

#[test]
#[ignore = "requires the independently pinned original FSharp XLIFF source list"]
fn original_xliff_sources_publish_markup_and_exact_bytes_after_reopen() {
    let list =
        std::env::var_os("ROOTLIGHT_XLF_SOURCE_LIST").expect("pinned XLIFF source list required");
    let list = fs::read_to_string(list).unwrap();
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut cases = Vec::new();
    for (index, original) in list.lines().enumerate() {
        let bytes = fs::read(original).unwrap();
        let path = format!("original{index}.xlf");
        fs::write(repository.path().join(&path), &bytes).unwrap();
        let start = bytes
            .windows(b"<xliff ".len())
            .position(|part| part == b"<xliff ")
            .unwrap()
            + 1;
        cases.push((path, bytes, start));
    }
    let expected_files = std::env::var("ROOTLIGHT_XLF_EXPECTED_FILES")
        .map_or(482, |value| value.parse::<usize>().unwrap());
    assert!(expected_files > 0);
    assert_eq!(cases.len(), expected_files);
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &deadline()).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(
        receipt.indexed_files,
        u64::try_from(expected_files).unwrap()
    );
    let assert_originals = |service: &FirstSliceService| {
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let doc = snapshot.document();
        for (path, bytes, start) in &cases {
            let file = doc.files.iter().find(|file| file.path == *path).unwrap();
            assert_eq!(file.language, "xml", "{path}");
            assert_eq!(file.content_hash, content_hash(bytes), "{path}");
            let budget = FirstSliceBudget::default();
            let maximum_page_bytes = budget
                .query()
                .max_source_bytes()
                .min(u64::try_from(budget.source().max_source_bytes).unwrap());
            let plan = paged_source::SourcePagePlan::new(
                file.evidence.source.clone().unwrap(),
                maximum_page_bytes,
            )
            .unwrap();
            let pages = paged_source::read_source_pages(&plan, |reference| {
                let read = service.source_read_with_options_and_budget(
                    receipt.generation,
                    vec![reference.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(rootlight_source::SourceEncoding::Bytes),
                    budget,
                    &deadline(),
                )?;
                let range = usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap();
                assert_eq!(read.data.chunks[0].bytes, bytes[range], "{path}");
                Ok(read)
            })
            .unwrap();
            assert_eq!(pages.page_count(), plan.page_count());
            let root = doc
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::MarkupElement
                        && entity.canonical_name == "xliff"
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().file() == file.id)
                })
                .unwrap();
            assert!(
                doc.occurrences.iter().any(|site| site.role
                    == rootlight_ir::OccurrenceRole::Definition
                    && site.source.span().file() == file.id
                    && site.source.span().start_byte() == u64::try_from(*start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(*start + 5).unwrap()
                    && site.target == rootlight_ir::OccurrenceTarget::Resolved { symbol: root.id }),
                "{path}"
            );
        }
    };
    assert_originals(&service);
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        receipt.generation
    );
    drop(service);
    let mut reopened = FirstSliceService::new_durable(2, paths.state_dir(), &deadline()).unwrap();
    assert_originals(&reopened);
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        receipt.generation
    );
    println!(
        "original-xliff\t{}\t{}",
        cases.len(),
        cases.iter().map(|(_, bytes, _)| bytes.len()).sum::<usize>()
    );
}

fn utf16le(text: &str) -> Vec<u8> {
    std::iter::once(0xfeff_u16)
        .chain(text.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn source_cases() -> Vec<(&'static str, Vec<u8>, &'static str, bool)> {
    let bad = "<?xml version='1.0' encoding='ISO-8859-1'?><é marker='yes'/>";
    vec![
        (
            "good.xml",
            b"<?xml version='1.0' encoding='UTF-8'?><kept/>".to_vec(),
            "utf-8",
            true,
        ),
        (
            "ascii.xml",
            b"<?xml version='1.0' encoding='ISO-8859-1'?><kept/>".to_vec(),
            "utf-8",
            true,
        ),
        (
            "utf16.xml",
            utf16le("<?xml version='1.0' encoding='UTF-16'?><kept/>"),
            "utf-16le",
            true,
        ),
        ("bad.xml", bad.as_bytes().to_vec(), "utf-8", false),
        ("explicit.data", bad.as_bytes().to_vec(), "utf-8", false),
        (
            "late.xml",
            format!(
                "<?xml version='1.0' encoding='ISO-8859-1'?><kept>{}é</kept>",
                "a".repeat(9000)
            )
            .into_bytes(),
            "utf-8",
            false,
        ),
        (
            "wrong-endian.xml",
            utf16le("<?xml version='1.0' encoding='UTF-16BE'?><kept/>"),
            "utf-16le",
            false,
        ),
        (
            "bom.xml",
            [
                b"\xef\xbb\xbf".as_slice(),
                b"<?xml version='1.0' encoding='ISO-8859-1'?><kept/>",
            ]
            .concat(),
            "utf-8",
            false,
        ),
    ]
}

fn assert_sources(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    cases: &[(&str, Vec<u8>, &str, bool)],
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    assert_eq!(receipt.indexed_files, u64::try_from(cases.len()).unwrap());
    for (path, bytes, encoding, accepted) in cases {
        let file = doc.files.iter().find(|file| file.path == *path).unwrap();
        assert_eq!(file.language, "xml", "{path}");
        assert_eq!(file.encoding, *encoding, "{path}");
        assert_eq!(file.content_hash, content_hash(bytes), "{path}");
        let source = file.evidence.source.clone().unwrap();
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![source],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(rootlight_source::SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, *bytes, "{path}");
        let entities = doc
            .entities
            .iter()
            .filter(|entity| {
                entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file.id)
            })
            .collect::<Vec<_>>();
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap();
        if *accepted {
            assert!(
                entities
                    .iter()
                    .any(|entity| entity.kind == EntityKind::MarkupElement),
                "{path}"
            );
            assert_ne!(coverage.status, CoverageStatus::Unknown, "{path}");
            assert!(
                !doc.diagnostics.iter().any(|diagnostic| {
                    diagnostic
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
                        && diagnostic.code == "unsupported-encoding"
                }),
                "{path}"
            );
        } else {
            assert!(entities.is_empty(), "{path}");
            assert!(
                !doc.occurrences
                    .iter()
                    .any(|site| site.source.span().file() == file.id),
                "{path}"
            );
            assert!(
                !doc.relations.iter().any(|relation| {
                    relation
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
                }),
                "{path}"
            );
            assert_eq!(coverage.status, CoverageStatus::Unknown, "{path}");
            assert_eq!(
                coverage.reason,
                Some(FirstSliceSourceCoverageReason::UnsupportedEncoding),
                "{path}"
            );
            assert!(
                doc.diagnostics.iter().any(|diagnostic| {
                    diagnostic
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
                        && diagnostic.code == "unsupported-encoding"
                        && diagnostic.message.contains("XML declaration reason")
                }),
                "{path}"
            );
        }
    }
}

#[test]
fn mixed_xml_declarations_preserve_sources_and_siblings_through_durable_lifecycle() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let cases = source_cases();
    for (path, bytes, _, _) in &cases {
        fs::write(repository.path().join(path), bytes).unwrap();
    }
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'explicit.data'='xml'\n"), 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_sources(&service, &initial, &cases);
    let noop = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(noop.generation, initial.generation);

    let mut changed = cases.clone();
    let bad = changed
        .iter_mut()
        .find(|(path, _, _, _)| *path == "bad.xml")
        .unwrap();
    bad.1 = std::str::from_utf8(&bad.1)
        .unwrap()
        .replace("ISO-8859-1", "UTF-8")
        .into_bytes();
    bad.3 = true;
    fs::write(repository.path().join(bad.0), &bad.1).unwrap();
    let updated = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(updated.generation, initial.generation);
    assert_sources(&service, &updated, &changed);
    let work = service.incremental_evidence(updated.generation).unwrap();
    assert_eq!(work.parsed_files(), 1);
    drop(service);

    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_sources(&reopened, &updated, &changed);
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        updated.generation
    );
    let original = cases
        .iter()
        .find(|(path, _, _, _)| *path == "bad.xml")
        .unwrap();
    fs::write(repository.path().join(original.0), &original.1).unwrap();
    let restored = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_sources(&reopened, &restored, &cases);
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_sources(&final_open, &restored, &cases);
    assert_eq!(
        final_open
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn xml_declaration_certificate_binds_provider_source_and_strict_admission() {
    let repository = durable_test_tempdir();
    let bytes = b"<?xml version='1.0' encoding='ISO-8859-1'?><\xc3\xa9/>";
    fs::write(repository.path().join("source.xml"), bytes).unwrap();
    let id = derive_repository(b"xml-encoding-certificate").id();
    let root = RepositoryRoot::open(id, repository.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.xml")).unwrap(),
            64 * 1024,
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        id,
        GenerationId::from_bytes([83; 20]),
        &snapshot,
        false,
        "xml",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    let provider = TreeSitterProvider::new_deferred(
        parser_config(service.analysis_limits.max_source_bytes()).unwrap(),
    )
    .unwrap();
    let proof = provider
        .certify_native_xml_encoding_refusal(
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap()
        .unwrap();
    assert!(proof.was_issued_by(&provider));
    assert!(proof.matches_request(&request));
    assert!(proof.matches_error(&proof.adapter_error()));
    assert_eq!(
        proof.reason(),
        rootlight_adapter_treesitter::XmlDeclarationError::AsciiProofRequired
    );
    assert_eq!(provider.stats().pooled_parsers, 0);
    assert_eq!(provider.stats().cache.entries, 0);
    let foreign = TreeSitterProvider::new_deferred(
        parser_config(service.analysis_limits.max_source_bytes()).unwrap(),
    )
    .unwrap();
    assert!(!proof.was_issued_by(&foreign));
    let other = structural_analysis_request(
        id,
        GenerationId::from_bytes([84; 20]),
        &snapshot,
        false,
        "xml",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap()
    .to_parse_request();
    assert!(!proof.matches_request(&other));
    assert!(!proof.matches_source(proof.source(), "html", "utf-8"));
    assert!(!proof.matches_source(proof.source(), "xml", "utf-16le"));
    assert!(matches!(
        provider.required_syntax_fact_count(&request, &Cancellation::new()),
        Err(AdapterError::RejectedRequest(
            rootlight_adapter_sdk::RequestError::UnsupportedEncoding
        ))
    ));
    assert!(matches!(
        provider.certify_native_xml_encoding_refusal(
            &request,
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            &Cancellation::new()
        ),
        Err(AdapterError::RejectedRequest(
            rootlight_adapter_sdk::RequestError::MemoryEnforcementUnavailable
        ))
    ));
    let cancelled = Cancellation::new();
    cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        provider.certify_native_xml_encoding_refusal(
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
#[ignore = "requires the independently pinned original Gobo checkout"]
fn original_eiffel_xml_companions_publish_written_markup_and_exact_sources() {
    let original = PathBuf::from(
        std::env::var_os("ROOTLIGHT_EIFFEL_REPOSITORY")
            .expect("set ROOTLIGHT_EIFFEL_REPOSITORY to the pinned Gobo checkout"),
    );
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut cases = Vec::new();
    for (index, (relative, element)) in [
        ("tool/geant/example/export/a.eant", "project"),
        ("library/common/config/libraries.eant", "project"),
        ("library/argument/library.ecf", "redirection"),
        (
            "tool/gecop/validation/validity/vycq9/test1/system.ecf",
            "system",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let bytes = fs::read(original.join(relative)).unwrap();
        let suffix = if relative.ends_with(".eant") {
            "eant"
        } else {
            "ecf"
        };
        let path = format!("original{index}.{suffix}");
        fs::write(repository.path().join(&path), &bytes).unwrap();
        let token = format!("<{element}");
        let start = bytes
            .windows(token.len())
            .position(|part| part == token.as_bytes())
            .unwrap()
            + 1;
        cases.push((path, bytes, element, start));
    }
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &deadline()).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.indexed_files, 4);
    let assert_originals = |service: &FirstSliceService| {
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let doc = snapshot.document();
        for (path, bytes, element, start) in &cases {
            let file = doc.files.iter().find(|file| file.path == *path).unwrap();
            assert_eq!(file.language, "xml", "{path}");
            assert_eq!(file.content_hash, content_hash(bytes), "{path}");
            let source = file.evidence.source.clone().unwrap();
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![source],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(rootlight_source::SourceEncoding::Bytes),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(read.data.chunks[0].bytes, *bytes, "{path}");
            let entity = doc
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::MarkupElement
                        && entity.canonical_name == *element
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().file() == file.id)
                })
                .unwrap();
            assert!(
                doc.occurrences.iter().any(|site| {
                    site.role == rootlight_ir::OccurrenceRole::Definition
                        && site.source.span().file() == file.id
                        && site.source.span().start_byte() == u64::try_from(*start).unwrap()
                        && site.source.span().end_byte()
                            == u64::try_from(*start + element.len()).unwrap()
                        && site.target
                            == rootlight_ir::OccurrenceTarget::Resolved { symbol: entity.id }
                }),
                "{path}"
            );
            assert!(
                !doc.entities.iter().any(|entity| {
                    entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
                        && matches!(entity.kind, EntityKind::Function | EntityKind::Class)
                }),
                "{path}"
            );
        }
    };
    assert_originals(&service);
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        receipt.generation
    );
    drop(service);
    let mut reopened = FirstSliceService::new_durable(2, paths.state_dir(), &deadline()).unwrap();
    assert_originals(&reopened);
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        receipt.generation
    );
}
