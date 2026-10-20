//! Public Racket analysis preserves written bindings and direct lexical calls.
//! Reader extensions, evaluated data and dynamic binding remain explicit gaps.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId, GenerationBoundSnapshot,
    LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, FactRef,
    IrLimits, OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint,
    RelationPredicate, SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = include_str!("fixtures/racket/whitespace.rkt");

fn analyze(text: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    analyze_raw(text.as_bytes(), "utf-8")
}

fn analyze_raw(raw: &[u8], encoding: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.rkt"), raw).unwrap();
    let repository = derive_repository(b"racket-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.rkt")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = TreeSitterProvider::new(
        RuntimeConfig::new(
            1024 * 1024,
            16_384,
            128,
            16,
            64,
            1,
            16 * 1024 * 1024,
            ParserSettings::new(256).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(provider),
        ProducerIdentity::new(
            "racket-source-contract",
            "1.0",
            content_hash(b"racket-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("racket").unwrap(),
        "racket-static-contract",
        content_hash(b"racket-contract-binary"),
    )
    .unwrap();
    let batch = BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        4096,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        batch,
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("racket").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"racket-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let result = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )
    .unwrap();
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), content_hash(raw));
        assert_eq!(occurrence.source.repository(), repository);
        assert_eq!(occurrence.source.generation(), source.generation());
        assert!(occurrence.source.span().end_byte() <= u64::try_from(raw.len()).unwrap());
    }
    result
}

fn entity<'a>(
    output: &'a rootlight_adapter_sdk::AnalysisOutput,
    name: &str,
) -> &'a rootlight_ir::EntityRecord {
    let values = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name && entity.kind == EntityKind::Function)
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 1, "{name}: {:?}", output.document().entities);
    values[0]
}

fn calls(output: &rootlight_adapter_sdk::AnalysisOutput) -> Vec<&rootlight_ir::RelationRecord> {
    output
        .document()
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Calls)
        .collect()
}

fn assert_call(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    source: &str,
    from: &str,
    to: &str,
    marker: &str,
) {
    let owner = entity(output, from);
    let target = entity(output, to);
    let relations = calls(output)
        .into_iter()
        .filter(|relation| {
            relation.subject == RelationEndpoint::Entity(owner.id)
                && relation.object == RelationEndpoint::Entity(target.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        relations.len(),
        1,
        "{from} -> {to}: {:?}",
        output.document().relations
    );
    let relation = relations[0];
    let witness = relation.evidence.source.as_ref().unwrap();
    let start = source.find(marker).unwrap() + marker.find(to).unwrap();
    assert_eq!(witness.span().start_byte(), u64::try_from(start).unwrap());
    assert_eq!(
        witness.span().end_byte(),
        u64::try_from(start + to.len()).unwrap()
    );
    let occurrences = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.enclosing == Some(owner.id)
                && occurrence.target == OccurrenceTarget::Resolved { symbol: target.id }
                && occurrence.source == *witness
        })
        .collect::<Vec<_>>();
    assert_eq!(occurrences.len(), 1);
    let mut proof = vec![
        FactRef::Entity(owner.id),
        FactRef::Fact(occurrences[0].id),
        FactRef::Entity(target.id),
    ];
    proof.sort_unstable();
    proof.dedup();
    assert_eq!(relation.evidence.derivation, proof);
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
}

#[test]
fn pollen_written_definitions_and_all_three_direct_calls_keep_exact_proof() {
    let output = analyze(SOURCE);
    for name in [
        "whitespace-base",
        "whitespace?",
        "not-whitespace?",
        "whitespace/nbsp?",
    ] {
        let record = entity(&output, name);
        let definitions = output
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: record.id }
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{name}");
        let start = SOURCE.find(&format!("(define ({name} ")).unwrap() + "(define (".len();
        assert_eq!(
            definitions[0].source.span().start_byte(),
            u64::try_from(start).unwrap()
        );
        assert_eq!(
            definitions[0].source.span().end_byte(),
            u64::try_from(start + name.len()).unwrap()
        );
        assert_eq!(
            definitions[0].syntactic_text_hash,
            content_hash(name.as_bytes())
        );
    }
    assert_call(
        &output,
        SOURCE,
        "whitespace?",
        "whitespace-base",
        "(whitespace-base x #:nbsp-is-white? #f)",
    );
    assert_call(
        &output,
        SOURCE,
        "not-whitespace?",
        "whitespace?",
        "(not (whitespace? x))",
    );
    assert_call(
        &output,
        SOURCE,
        "whitespace/nbsp?",
        "whitespace-base",
        "(whitespace-base x #:nbsp-is-white? #t)",
    );
    assert_eq!(&SOURCE[514..529], "whitespace-base");
    assert!(
        !output.document().occurrences.iter().any(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.source.span().start_byte() == 514
                && occurrence.source.span().end_byte() == 529
        }),
        "the authored direct-call witness is not another definition"
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "racket-reader-macro-import-and-runtime-binding-unavailable")
    );
    assert_eq!(
        SOURCE
            .match_indices("whitespace-base")
            .map(|(start, _)| start)
            .collect::<Vec<_>>(),
        [83, 514, 641]
    );
    assert!(!SOURCE.contains("whitespace-base__absent_source_control__"));
}

#[test]
fn reader_data_comments_literals_and_syntax_templates_do_not_declare_or_call() {
    let source = "#lang racket/base\n(define (target x) x)\n; (define (commented x) (target x))\n#;(define (discarded x) (target x))\n'(define (quoted x) (target x))\n#'(define (syntax-only x) (target x))\n`(define (template x) ,(target x))\n(quote (define (long-quoted x) (target x)))\n\"(define (string-only x) (target x))\"\n#<<END\nEXTRA (define (here-only x) (target x))\nEND\n(define (syntax-data x) (quote-syntax (target x)))\n(define (opaque x) (unknown-form (target x)))\n(define (live x) (target x))\n";
    let output = analyze(source);
    for absent in [
        "commented",
        "discarded",
        "quoted",
        "syntax-only",
        "template",
        "long-quoted",
        "string-only",
        "here-only",
    ] {
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|record| record.canonical_name == absent),
            "{absent}"
        );
    }
    assert_eq!(calls(&output).len(), 1);
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "racket-application-argument-or-macro-context-unavailable")
    );
    assert_call(
        &output,
        source,
        "live",
        "target",
        "(define (live x) (target x))",
    );
}

#[test]
fn lexical_parameter_let_named_let_and_duplicate_targets_do_not_fabricate_calls() {
    for body in [
        "(define (caller target) (target 1))",
        "(define (caller x) (let ([target (lambda (v) v)]) (target x)))",
        "(define (caller x) (let target ([v x]) (target v)))",
        "(define (caller x) (define-syntaxes (target) (values (lambda (stx) #'0))) (target x))",
        "(define (target y) y)\n(define (caller x) (target x))",
    ] {
        let source = format!("#lang racket/base\n(define (target x) x)\n{body}\n");
        let output = analyze(&source);
        assert!(
            calls(&output).is_empty(),
            "{body}: {:?}",
            output.document().relations
        );
        if body.contains("define-syntaxes") {
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| { gap.detail == "racket-dynamic-or-unmodeled-binding-unavailable" })
            );
        }
    }
}

#[test]
fn nested_written_functions_resolve_the_closest_lexical_binding() {
    let source = "#lang racket/base\n(define (target x) x)\n(define (outer x) (define (local y) y) (define (caller z) (local z)) (caller x))\n";
    let output = analyze(source);
    assert_call(&output, source, "caller", "local", "(local z)");
    assert_call(&output, source, "outer", "caller", "(caller x)");
}

#[test]
fn custom_readers_case_directives_and_dynamic_heads_remain_concrete_gaps() {
    for prefix in [
        "#lang scribble/manual",
        "#lang s-exp pollen/private/dialect",
        "#reader \"custom-reader.rkt\"",
        "#ci",
        "#cI",
        "#Ci",
        "#CI",
        "#cs",
        "#cS",
        "#Cs",
        "#CS",
    ] {
        let source = format!("{prefix}\n(define (fabricated x) x)\n");
        let output = analyze(&source);
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|record| record.kind == EntityKind::Function),
            "{prefix}"
        );
        assert!(calls(&output).is_empty());
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "racket-reader-language-or-case-environment-unavailable"),
            "{prefix}: {:?}",
            output.document().skipped_regions
        );
    }
    let source = "#lang racket/base\n(define ((computed) x) x)\n(define-syntax-rule (make name) (define (name x) x))\n(define (literal x) x)\n";
    let output = analyze(source);
    assert!(
        output
            .document()
            .entities
            .iter()
            .any(|record| record.canonical_name == "literal")
    );
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|record| matches!(record.canonical_name.as_str(), "computed" | "name"))
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "racket-dynamic-or-unmodeled-binding-unavailable")
    );
}

#[test]
fn utf16_unicode_prefixes_preserve_raw_definition_and_call_leaf_identity() {
    let source = "#lang racket/base\n; λ🙂 raw prefix\n(define (target? x) x)\n(define (caller x) (target? x))\n";
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        raw.extend(source.encode_utf16().flat_map(|unit| {
            if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            }
        }));
        let output = analyze_raw(&raw, if little { "utf-16le" } else { "utf-16be" });
        let target = entity(&output, "target?");
        let owner = entity(&output, "caller");
        for (record, written) in [(target, "target?"), (owner, "caller")] {
            let definitions = output
                .document()
                .occurrences
                .iter()
                .filter(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.target == OccurrenceTarget::Resolved { symbol: record.id }
                })
                .collect::<Vec<_>>();
            assert_eq!(definitions.len(), 1);
            let start = source.find(written).unwrap();
            let raw_start = 2 + 2 * source[..start].encode_utf16().count();
            let raw_end = raw_start + 2 * written.encode_utf16().count();
            assert_eq!(
                definitions[0].source.span().start_byte(),
                u64::try_from(raw_start).unwrap()
            );
            assert_eq!(
                definitions[0].source.span().end_byte(),
                u64::try_from(raw_end).unwrap()
            );
            let expected = written
                .encode_utf16()
                .flat_map(|unit| {
                    if little {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(&raw[raw_start..raw_end], expected);
            assert_eq!(definitions[0].source.content_hash(), content_hash(&raw));
            assert_eq!(
                definitions[0].syntactic_text_hash,
                content_hash(written.as_bytes())
            );
        }
        let relation = calls(&output)
            .into_iter()
            .find(|relation| {
                relation.subject == RelationEndpoint::Entity(owner.id)
                    && relation.object == RelationEndpoint::Entity(target.id)
            })
            .unwrap();
        let witness = relation.evidence.source.as_ref().unwrap();
        let start = source.rfind("target?").unwrap();
        let raw_start = 2 + 2 * source[..start].encode_utf16().count();
        let raw_end = raw_start + 2 * "target?".encode_utf16().count();
        assert_eq!(
            witness.span().start_byte(),
            u64::try_from(raw_start).unwrap()
        );
        assert_eq!(witness.span().end_byte(), u64::try_from(raw_end).unwrap());
        let expected = "target?"
            .encode_utf16()
            .flat_map(|unit| {
                if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(&raw[raw_start..raw_end], expected);
        assert_eq!(witness.content_hash(), content_hash(&raw));
        assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
    }
}

#[test]
fn custom_reader_header_is_complete_and_precedes_all_base_racket_body_facts() {
    for header in [
        "#lang pollen",
        "#lang scribble/manual",
        "#lang s-exp \"reader.rkt\"",
        "#lang reader \"custom-reader.rkt\"",
    ] {
        for ending in ["\n", "\r\n", ""] {
            let source = if ending.is_empty() {
                header.to_owned()
            } else {
                format!(
                    "{header}{ending}; apparent comment{ending}\"apparent string\"{ending}(define (phantom x) x){ending}"
                )
            };
            for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"] {
                let raw = match encoding {
                    "utf-8" => source.as_bytes().to_vec(),
                    "utf-8-bom" => [b"\xef\xbb\xbf".as_slice(), source.as_bytes()].concat(),
                    _ => std::iter::once(0xfeff_u16)
                        .chain(source.encode_utf16())
                        .flat_map(|unit| {
                            if encoding == "utf-16le" {
                                unit.to_le_bytes()
                            } else {
                                unit.to_be_bytes()
                            }
                        })
                        .collect(),
                };
                let output = analyze_raw(
                    &raw,
                    if encoding == "utf-8-bom" {
                        "utf-8"
                    } else {
                        encoding
                    },
                );
                assert!(
                    output
                        .document()
                        .entities
                        .iter()
                        .all(|entity| entity.kind == EntityKind::Module),
                    "{header}/{encoding}"
                );
                assert!(
                    output.document().occurrences.is_empty(),
                    "{header}/{encoding}: {:?}",
                    output.document().occurrences
                );
                assert!(calls(&output).is_empty());
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
                let gaps = output
                    .document()
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.detail.starts_with("racket-reader-")
                            && gap.source.span().start_byte() == start
                            && gap.source.span().end_byte()
                                == start + u64::try_from(expected_header.len()).unwrap()
                    })
                    .collect::<Vec<_>>();
                for domain in [
                    rootlight_ir::FactDomain::Entities,
                    rootlight_ir::FactDomain::Occurrences,
                    rootlight_ir::FactDomain::Relations,
                ] {
                    assert!(
                        gaps.iter().any(|gap| gap.domain == domain),
                        "{header}/{encoding}/{domain:?}"
                    );
                }
                for gap in gaps {
                    assert_eq!(gap.source.content_hash(), content_hash(&raw));
                    assert_eq!(gap.source.generation(), GenerationId::from_bytes([9; 20]));
                    let span = gap.source.span();
                    assert_eq!(
                        &raw[usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap()],
                        expected_header
                    );
                }
            }
        }
    }
}
