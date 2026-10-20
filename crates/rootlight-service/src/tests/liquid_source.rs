//! Known Liquid sources remain exact and named while analysis is unavailable.

use super::*;
use rootlight_ir::FactDomain as IrFactDomain;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop};

const ORIGINAL: &[u8] =
    include_bytes!("../../../rootlight-discovery/tests/fixtures/liquid_source/blog-sidebar.liquid");

fn assert_source(service: &FirstSliceService, receipt: &FirstSliceIndexReceipt, original: &[u8]) {
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let document = lease.generation().document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.language, "liquid");
    assert_eq!(file.path, "blog-sidebar.liquid");
    assert_eq!(file.content_hash, content_hash(original));
    assert_eq!(file.byte_length, u64::try_from(original.len()).unwrap());
    assert!(document.entities.is_empty());
    assert!(document.occurrences.is_empty());
    assert!(document.relations.is_empty());
    let reference = file.evidence.source.clone().unwrap();
    assert_eq!(reference.repository(), receipt.repository);
    assert_eq!(reference.generation(), receipt.generation);
    assert_eq!(reference.content_hash(), file.content_hash);
    assert_eq!(
        reference.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, reference);
    assert_eq!(read.data.chunks[0].bytes, original);
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == domain
                    && gap.detail == "unsupported-language"
                    && gap.source == reference)
        );
        assert!(document.coverage_records.iter().any(
            |coverage| coverage.domain == domain && coverage.status == CoverageStatus::Unknown
        ));
    }
    assert!(
        document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "unsupported-language"
                && diagnostic.source.as_ref() == Some(&reference))
    );
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(coverage.language, "liquid");
    assert_eq!(coverage.status, CoverageStatus::Unknown);
    assert_eq!(
        coverage.reason,
        Some(FirstSliceSourceCoverageReason::UnsupportedLanguage)
    );
    let located = service
        .code_locate(
            receipt.generation,
            file.path.clone(),
            LocateMode::Exact,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(
        located
            .data
            .hits
            .iter()
            .any(|hit| hit.path == file.path && hit.symbol.is_none())
    );
    let literal = "blogs.sidebar.recent_articles";
    let expected = original
        .windows(literal.len())
        .position(|part| part == literal.as_bytes())
        .unwrap();
    let found = service
        .find_literal(
            receipt.generation,
            file.id,
            literal,
            None,
            SourceFindBudget::new(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(found.stop, SourceFindStop::EndOfSource);
    assert!(found.continuation.is_none());
    assert_eq!(found.matches.len(), 1);
    assert_eq!(
        found.matches[0].span().start_byte(),
        u64::try_from(expected).unwrap()
    );
    assert_eq!(
        found.matches[0].span().end_byte(),
        u64::try_from(expected + literal.len()).unwrap()
    );
}

#[test]
fn original_liquid_source_stays_named_exact_and_explicitly_unanalyzed() {
    assert_eq!(ORIGINAL.len(), 888);
    for (start, end, token) in [
        (191, 198, b"article".as_slice()),
        (270, 277, b"article".as_slice()),
        (673, 676, b"tag".as_slice()),
        (753, 756, b"tag".as_slice()),
    ] {
        assert_eq!(&ORIGINAL[start..end], token);
    }
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    fs::write(fixture.path().join("blog-sidebar.liquid"), ORIGINAL).unwrap();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    assert!(!service.analyzers.contains_key("liquid"));
    let first = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_source(&service, &first, ORIGINAL);
    assert_eq!(
        service
            .index_repository(fixture.path(), &deadline())
            .unwrap(),
        first
    );
    let mut changed = ORIGINAL.to_vec();
    changed.extend_from_slice(b"\n{% comment %} exact-source-update {% endcomment %}\n");
    fs::write(fixture.path().join("blog-sidebar.liquid"), &changed).unwrap();
    let second = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_ne!(second.generation, first.generation);
    assert_source(&service, &second, &changed);
    drop(service);
    let reopened = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    assert_source(&reopened, &first, ORIGINAL);
    assert_source(&reopened, &second, &changed);
}

#[test]
fn liquid_suffix_does_not_admit_ruby_html_or_javascript_host_parsers() {
    for (name, source) in [
        (
            "script.js.liquid",
            "function apparent(value) { return value; }",
        ),
        (
            "page.html.liquid",
            "<!DOCTYPE html><html><body>{{ value }}</body></html>",
        ),
        (
            "script.ruby.liquid",
            "#!/usr/bin/env ruby\ndef apparent(value)\nvalue\nend\n",
        ),
    ] {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join(name), source).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let lease = service
            .generation_lease(receipt.generation, &deadline())
            .unwrap();
        let document = lease.generation().document();
        assert_eq!(document.files.len(), 1);
        assert_eq!(document.files[0].language, "liquid", "{name}");
        assert_eq!(
            document.files[0].content_hash,
            content_hash(source.as_bytes())
        );
        assert!(document.entities.is_empty(), "{name}");
        assert!(document.occurrences.is_empty(), "{name}");
        assert!(document.relations.is_empty(), "{name}");
        assert!(
            document
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "unsupported-language")
        );
    }
}

#[test]
fn liquid_outer_suffix_fences_schema_and_racket_hosts_with_exact_unknown_sources() {
    let sources = [
        (
            "page.json.liquid",
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"properties\":{\"value\":{\"type\":\"string\"}}}\n",
        ),
        (
            "script.rkt.liquid",
            "#lang racket/base\n(define apparent 1)\n",
        ),
    ];
    let fixture = TempDir::new().unwrap();
    for (path, source) in sources {
        fs::write(fixture.path().join(path), source).unwrap();
    }
    let mut service = FirstSliceService::new(2).unwrap();
    assert!(service.analyzers.contains_key("json-schema"));
    assert!(service.analyzers.contains_key("racket"));
    assert!(!service.analyzers.contains_key("liquid"));
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let lease = service
        .generation_lease(receipt.generation, &deadline())
        .unwrap();
    let document = lease.generation().document();
    assert_eq!(document.files.len(), 2);
    assert!(document.entities.is_empty());
    assert!(document.occurrences.is_empty());
    assert!(document.relations.is_empty());
    for (path, source) in sources {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "liquid", "{path}");
        assert_eq!(file.content_hash, content_hash(source.as_bytes()));
        assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
        let reference = file.evidence.source.clone().unwrap();
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), file.content_hash);
        assert_eq!(
            reference.span(),
            SourceSpan::new(file.id, 0, file.byte_length).unwrap()
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![reference.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, reference);
        assert_eq!(read.data.chunks[0].bytes, source.as_bytes());
        for domain in [
            IrFactDomain::Entities,
            IrFactDomain::Occurrences,
            IrFactDomain::Relations,
        ] {
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.domain == domain
                        && gap.detail == "unsupported-language"
                        && gap.source == reference)
            );
            assert!(
                document
                    .coverage_records
                    .iter()
                    .any(
                        |coverage| coverage.scope == rootlight_ir::CoverageScope::File(file.id)
                            && coverage.domain == domain
                            && coverage.status == CoverageStatus::Unknown
                    )
            );
        }
        assert!(
            document
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "unsupported-language"
                    && diagnostic.source.as_ref() == Some(&reference))
        );
    }
}

#[test]
fn liquid_content_metadata_preserves_configuration_and_real_host_routes() {
    let service = FirstSliceService::new(2).unwrap();
    for (path, real_host_path, content_language) in [
        ("page.json.liquid", "page.json", "json-schema"),
        ("script.rkt.liquid", "script.rkt", "racket"),
    ] {
        assert!(service.analyzers.contains_key(content_language));
        let input = ManifestInput {
            file: FileId::from_bytes([102; 20]),
            path: path.to_owned(),
            content_hash: content_hash(b"independent-liquid-routing-input"),
            bytes: u64::try_from(b"independent-liquid-routing-input".len()).unwrap(),
            encoding: "utf-8".to_owned(),
            class: InputClass::Source,
            language_signals: vec![
                rootlight_discovery::LanguageSignal {
                    language: "liquid".to_owned(),
                    evidence: LanguageEvidence::Extension,
                },
                rootlight_discovery::LanguageSignal {
                    language: content_language.to_owned(),
                    evidence: LanguageEvidence::Content,
                },
            ],
            project_language_witnesses: Vec::new(),
            decisive_rule: None,
        };
        let original_signals = input.language_signals.clone();
        assert_eq!(detected_source_language(&input), Some("liquid"));
        assert_eq!(source_language_route(&input, &service.analyzers), None);
        assert_eq!(supported_source_language(&input, &service.analyzers), None);
        assert_eq!(input.language_signals, original_signals);
        let mut host = input.clone();
        host.path = real_host_path.to_owned();
        host.language_signals.remove(0);
        assert_eq!(detected_source_language(&host), Some(content_language));
        assert_eq!(
            source_language_route(&host, &service.analyzers),
            Some(content_language)
        );
        for configured_language in ["liquid", content_language, "unavailable-liquid-parser"] {
            let mut configured = input.clone();
            configured
                .language_signals
                .push(rootlight_discovery::LanguageSignal {
                    language: configured_language.to_owned(),
                    evidence: LanguageEvidence::Configuration,
                });
            assert_eq!(
                detected_source_language(&configured),
                Some(configured_language)
            );
            assert_eq!(
                source_language_route(&configured, &service.analyzers),
                service
                    .analyzers
                    .contains_key(configured_language)
                    .then_some(configured_language)
            );
        }
        let mut conflicting = input.clone();
        for language in ["liquid", content_language] {
            conflicting
                .language_signals
                .push(rootlight_discovery::LanguageSignal {
                    language: language.to_owned(),
                    evidence: LanguageEvidence::Configuration,
                });
        }
        assert_eq!(detected_source_language(&conflicting), None);
        assert_eq!(
            source_language_route(&conflicting, &service.analyzers),
            None
        );
    }
}
