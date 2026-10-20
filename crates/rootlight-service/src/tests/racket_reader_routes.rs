//! Reader declarations route retained source independently of filename defaults.
//! Custom readers retain exact header gaps without borrowing base Racket facts.

use super::*;
use rootlight_discovery::LanguageSignal;

#[test]
fn verified_reader_content_precedes_perl_suffix_but_preserves_overrides_and_conflicts() {
    let service = FirstSliceService::new(2).unwrap();
    let mut input = ManifestInput {
        file: FileId::from_bytes([3; 20]),
        path: "chapter.html.pm".to_owned(),
        content_hash: content_hash(b"#lang pollen\n"),
        bytes: 13,
        encoding: "utf-8".to_owned(),
        class: InputClass::Source,
        language_signals: vec![
            LanguageSignal {
                language: "perl".to_owned(),
                evidence: LanguageEvidence::Extension,
            },
            LanguageSignal {
                language: "racket".to_owned(),
                evidence: LanguageEvidence::Content,
            },
        ],
        project_language_witnesses: Vec::new(),
        decisive_rule: None,
    };
    assert_eq!(detected_source_language(&input), Some("racket"));
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("racket")
    );
    input.language_signals.push(LanguageSignal {
        language: "perl".to_owned(),
        evidence: LanguageEvidence::Configuration,
    });
    assert_eq!(detected_source_language(&input), Some("perl"));
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("perl")
    );
    input.language_signals.push(LanguageSignal {
        language: "racket".to_owned(),
        evidence: LanguageEvidence::Configuration,
    });
    assert_eq!(detected_source_language(&input), None);
    assert_eq!(source_language_route(&input, &service.analyzers), None);
    input.language_signals.truncate(2);
    for evidence in [LanguageEvidence::Content, LanguageEvidence::Shebang] {
        input.language_signals.push(LanguageSignal {
            language: "python".to_owned(),
            evidence,
        });
        assert_eq!(detected_source_language(&input), None);
        assert_eq!(source_language_route(&input, &service.analyzers), None);
        input.language_signals.pop();
    }
    input.language_signals.pop();
    assert_eq!(detected_source_language(&input), Some("perl"));
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("perl")
    );
}

#[test]
fn custom_reader_files_keep_original_header_evidence_without_base_language_facts() {
    let temporary = tempfile::tempdir().unwrap();
    let header = "#lang s-exp \"reader.rkt\"";
    let text = format!(
        "{header}\r\n; apparent comment\r\n\"apparent string\"\r\n(define (phantom x) x)\r\n"
    );
    let mut cases = Vec::new();
    for (name, encoding) in [
        ("page.pm", "utf-8"),
        ("document.scrbl", "utf-8-bom"),
        ("little.no-ext", "utf-16le"),
        ("big.pp", "utf-16be"),
    ] {
        let encode = |text: &str| -> Vec<u8> {
            match encoding {
                "utf-8" => text.as_bytes().to_vec(),
                "utf-8-bom" => [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(),
                _ => std::iter::once(0xfeff_u16)
                    .chain(text.encode_utf16())
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect(),
            }
        };
        let bytes = encode(&text);
        let expected_header = match encoding {
            "utf-8" | "utf-8-bom" => header.as_bytes().to_vec(),
            _ => header
                .encode_utf16()
                .flat_map(|unit| {
                    if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect(),
        };
        let start = match encoding {
            "utf-8" => 0,
            "utf-8-bom" => 3,
            _ => 2,
        };
        fs::write(temporary.path().join(name), &bytes).unwrap();
        cases.push((name, bytes, expected_header, start));
    }
    fs::write(
        temporary.path().join("ordinary.pm"),
        b"package Ordinary; sub value { return 1; }\n1;\n",
    )
    .unwrap();
    fs::write(
        temporary.path().join("base.reader"),
        b"#lang racket/base\n(define (literal x) x)\n",
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(temporary.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(
        document
            .files
            .iter()
            .find(|file| file.path == "ordinary.pm")
            .unwrap()
            .language,
        "perl"
    );
    let base = document
        .files
        .iter()
        .find(|file| file.path == "base.reader")
        .unwrap();
    assert_eq!(base.language, "racket");
    assert!(document.entities.iter().any(|entity| {
        entity.kind == EntityKind::Function
            && entity.canonical_name == "literal"
            && entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == base.id)
    }));
    for (name, bytes, expected_header, start) in cases {
        let file = document
            .files
            .iter()
            .find(|file| file.path == name)
            .unwrap();
        assert_eq!(file.language, "racket", "{name}");
        assert_eq!(file.content_hash, content_hash(&bytes));
        assert!(!document.entities.iter().any(|entity| {
            entity.kind != EntityKind::Module
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file.id)
        }));
        assert!(
            !document
                .occurrences
                .iter()
                .any(|occurrence| occurrence.source.span().file() == file.id)
        );
        let gap = document
            .skipped_regions
            .iter()
            .find(|gap| {
                gap.source.span().file() == file.id
                    && gap.detail == "racket-reader-language-or-case-environment-unavailable"
            })
            .unwrap();
        assert_eq!(gap.source.span().start_byte(), start);
        assert_eq!(
            gap.source.span().end_byte(),
            start + u64::try_from(expected_header.len()).unwrap()
        );
        assert_eq!(gap.source.content_hash(), content_hash(&bytes));
        assert_eq!(gap.source.generation(), receipt.generation);
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![gap.source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(rootlight_source::SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, gap.source);
        assert_eq!(read.data.chunks[0].bytes, expected_header);
    }
}
