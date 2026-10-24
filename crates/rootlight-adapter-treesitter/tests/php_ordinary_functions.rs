//! Complete licensed PHP functions retain physical same-owner formal bindings.
//! Written dependencies do not claim external execution or configured runtime behavior.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, EntityKind, ExtensionSupport, IrLimits,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate,
    SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const ORIGINAL: &str = include_str!("fixtures/php_ordinary_functions/geo_helpers.php");

fn analyze(source: &str) -> AnalysisOutput {
    analyze_raw(source.as_bytes(), "utf-8")
}

fn analyze_raw(bytes: &[u8], encoding: &str) -> AnalysisOutput {
    analyze_observed(bytes, encoding, |_, _, _| Ok(())).unwrap()
}

fn analyze_observed(
    bytes: &[u8],
    encoding: &str,
    observe: impl FnOnce(
        &TreeSitterAnalyzer,
        &TreeSitterProvider,
        &AnalysisRequest<'_>,
    ) -> Result<(), rootlight_adapter_sdk::AdapterError>,
) -> Result<AnalysisOutput, rootlight_adapter_sdk::AdapterError> {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.php"), bytes).unwrap();
    let repository = derive_repository(b"php-ordinary-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.php")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(bytes.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(
        TreeSitterProvider::new(
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
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "php-ordinary-contract",
            "1.0",
            content_hash(b"php-configuration"),
        )
        .unwrap(),
        LanguageId::new("php").unwrap(),
        "php-ordinary-source",
        content_hash(b"php-contract-binary"),
    )
    .unwrap();
    let stream = StreamLimits::new(
        128,
        4096,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
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
        LanguageId::new("php").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"php-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    observe(&analyzer, provider.as_ref(), &request)?;
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )?;
    validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(output.document().files.len(), 1);
    assert_eq!(output.document().files[0].content_hash, content_hash(bytes));
    assert_eq!(
        output.document().files[0].byte_length,
        u64::try_from(bytes.len()).unwrap()
    );
    for occurrence in &output.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), source.content_hash());
        assert_eq!(occurrence.source.repository(), source.repository());
        assert_eq!(occurrence.source.generation(), source.generation());
        assert!(occurrence.source.span().end_byte() <= u64::try_from(bytes.len()).unwrap());
    }
    Ok(output)
}

fn defined(output: &AnalysisOutput, kind: EntityKind, start: u64, end: u64) -> SymbolId {
    let document = output.document();
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact original Definition");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("definition resolves")
    };
    assert_eq!(
        document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap()
            .kind,
        kind
    );
    symbol
}

fn original_obligations(output: &AnalysisOutput) {
    let document = output.document();
    let owner = defined(output, EntityKind::Function, 346, 357);
    let caller = defined(output, EntityKind::Function, 681, 700);
    let parameter = defined(output, EntityKind::Parameter, 358, 362);
    let formal = document
        .entities
        .iter()
        .find(|entity| entity.id == parameter)
        .unwrap();
    assert_eq!(formal.canonical_name, "$lat");
    assert_eq!(formal.container, Some(ContainerRef::Entity(owner)));
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 440
                && site.source.span().end_byte() == 444
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: parameter }
    );
    assert_eq!(sites[0].enclosing, Some(owner));
    assert_eq!(sites[0].provenance, formal.provenance);
    let contains: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(owner)
                && edge.object == RelationEndpoint::Entity(parameter)
        })
        .collect();
    assert_eq!(contains.len(), 1, "function owns its exact formal");
    let calls: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(caller)
                && edge.object == RelationEndpoint::Entity(owner)
                && edge.evidence.source.as_ref().is_some_and(|source| {
                    source.span().start_byte() == 838 && source.span().end_byte() == 849
                })
        })
        .collect();
    assert_eq!(calls.len(), 1, "one exact written function dependency");
    assert_eq!(calls[0].id, derive_relation_record_id(calls[0]).unwrap());
}

#[test]
fn complete_original_ordinary_functions_keep_four_missing_source_obligations() {
    assert_eq!(ORIGINAL.len(), 875);
    assert_eq!(&ORIGINAL.as_bytes()[358..362], b"$lat");
    assert_eq!(&ORIGINAL.as_bytes()[440..444], b"$lat");
    assert_eq!(&ORIGINAL.as_bytes()[838..849], b"fallbackBox");
    original_obligations(&analyze(ORIGINAL));
}

#[test]
fn same_named_formals_use_their_exact_function_and_php_function_case_rules() {
    let source = "<?php function target($lat) { return $lat; } function caller($lat) { return TaRgEt($lat); }";
    let output = analyze(source);
    let document = output.document();
    let parameters: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter)
        .collect();
    assert_eq!(parameters.len(), 2);
    assert_ne!(parameters[0].id, parameters[1].id);
    assert_ne!(parameters[0].container, parameters[1].container);
    for site in document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "php.ordinary_formal.reference")
    {
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("own formal resolves")
        };
        let parameter = parameters
            .iter()
            .find(|parameter| parameter.id == symbol)
            .unwrap();
        assert_eq!(
            parameter.container,
            site.enclosing.map(ContainerRef::Entity)
        );
    }
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
    let output = analyze("<?php function target($lat, $Lat) { return $lat + $Lat; }");
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        2
    );
}

#[test]
fn rejected_function_forms_never_acquire_parameters_or_borrow_another_owner() {
    for source in [
        "<?php function good($lat) { return $lat; } function bad($lat) { $lat = 0; return $lat; }",
        "<?php function good($lat) { return $lat; } function bad(&$lat) { return $lat; }",
        "<?php function good($lat) { return $lat; } function bad(...$lat) { return $lat; }",
        "<?php function good($lat) { return $lat; } function bad($lat) { global $lat; return $lat; }",
        "<?php function good($lat) { return $lat; } function bad($lat) { unset($lat); return $lat; }",
        "<?php function good($lat) { return $lat; } function bad($lat) { return fn() => $lat; }",
    ] {
        let output = analyze(source);
        let document = output.document();
        let bad = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "bad")
            .unwrap();
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Parameter
                    && entity.container == Some(ContainerRef::Entity(bad.id))),
            "{source}"
        );
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Reference
                    && site.enclosing == Some(bad.id)
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
    }
    for source in [
        "<?php function target($lat) {} function TARGET($lng) {}",
        "<?php namespace Place; function target($lat) { return $lat; }",
        "<?php if (true) { function target($lat) { return $lat; } }",
        "<?php class Place { function target($lat) { return $lat; } }",
        "<?php function target($lat) { require 'other.php'; return $lat; }",
        "<?php function target($lat) { eval('changed'); return $lat; }",
    ] {
        let output = analyze(source);
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Parameter),
            "{source}"
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls),
            "{source}"
        );
    }
}

#[test]
fn original_references_full_calls_and_external_uncertainty_survive_the_added_call_facet() {
    let output = analyze(ORIGINAL);
    original_obligations(&output);
    let document = output.document();
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "php.name.reference"
                && site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 838
                && site.source.span().end_byte() == 849)
            .count(),
        1
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "php.call.call"
                && site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == 838
                && site.source.span().end_byte() == 869)
            .count(),
        1
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "php.ordinary_function_call.call")
            .count(),
        1
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
    for range in [(399, 664), (738, 751)] {
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::CallSite
                    && site.source.span().start_byte() == range.0
                    && site.source.span().end_byte() == range.1
            })
            .unwrap();
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    }
    assert!(
        document.skipped_regions.iter().any(
            |gap| gap.detail == "php-written-function-runtime-and-external-effects-unavailable"
        )
    );
}

#[test]
fn physical_utf8_bom_and_utf16_keep_exact_parameter_reference_and_call_witnesses() {
    for encoding in ["utf-8-bom", "utf-16le", "utf-16be"] {
        let (raw, prefix, unit): (Vec<u8>, u64, u64) = if encoding == "utf-8-bom" {
            (
                [b"\xef\xbb\xbf".as_slice(), ORIGINAL.as_bytes()].concat(),
                3,
                1,
            )
        } else {
            (
                std::iter::once(0xfeff_u16)
                    .chain(ORIGINAL.encode_utf16())
                    .flat_map(|value| {
                        if encoding == "utf-16le" {
                            value.to_le_bytes()
                        } else {
                            value.to_be_bytes()
                        }
                    })
                    .collect(),
                2,
                2,
            )
        };
        let output = analyze_raw(
            &raw,
            if encoding == "utf-8-bom" {
                "utf-8"
            } else {
                encoding
            },
        );
        let parameter = defined(
            &output,
            EntityKind::Parameter,
            prefix + 358 * unit,
            prefix + 362 * unit,
        );
        assert!(output.document().occurrences.iter().any(|site| site.role
            == OccurrenceRole::Reference
            && site.source.span().start_byte() == prefix + 440 * unit
            && site.source.span().end_byte() == prefix + 444 * unit
            && site.target == OccurrenceTarget::Resolved { symbol: parameter }));
        let call = output
            .document()
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "php.ordinary_function_call.call")
            .unwrap();
        assert_eq!(call.source.span().start_byte(), prefix + 838 * unit);
        assert_eq!(call.source.span().end_byte(), prefix + 849 * unit);
        assert_eq!(call.source.content_hash(), content_hash(&raw));
        assert!(
            output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "php.call.call"
                    && site.source.span().start_byte() == prefix + 838 * unit
                    && site.source.span().end_byte() == prefix + 869 * unit)
        );
    }
}

#[test]
fn native_identity_output_has_one_formal_reference_and_preserves_both_call_extents() {
    use rootlight_adapter_sdk::{SyntaxFactKind, execute_parse};
    analyze_observed(ORIGINAL.as_bytes(), "utf-8", |_, provider, request| {
        let parsed = execute_parse(
            provider,
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )?;
        for (label, start, end) in [
            ("php.ordinary_formal.reference", 440, 444),
            ("php.ordinary_formal_target.signature", 440, 444),
            ("php.name.reference", 838, 849),
            ("php.call.call", 838, 869),
            ("php.ordinary_call_target.signature", 838, 849),
            ("php.ordinary_call_context.signature", 838, 869),
        ] {
            assert_eq!(
                parsed
                    .facts()
                    .iter()
                    .filter(|fact| fact.syntax_kind().as_str() == label
                        && fact.span().start_byte() == start
                        && fact.span().end_byte() == end)
                    .count(),
                1,
                "{label}"
            );
        }
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| fact.kind() == SyntaxFactKind::Declaration
                    && fact.syntax_kind().as_str() == "php.ordinary_parameter.declaration")
                .count(),
            6
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn missing_or_duplicate_formal_certificate_keeps_parameter_but_withholds_reference_binding() {
    use rootlight_adapter_sdk::{
        ParseProvider, SyntaxFact, execute_parse, testkit::MockParseProvider,
    };
    analyze_observed(ORIGINAL.as_bytes(), "utf-8", |_, provider, request| {
        let cancel = Cancellation::new();
        let parsed = execute_parse(
            provider,
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel,
        )?;
        let marker = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == "php.ordinary_formal_target.signature"
                    && fact.span().start_byte() == 440
            })
            .unwrap();
        for duplicate in [false, true] {
            let mut facts = parsed.facts().to_vec();
            if duplicate {
                facts.push(SyntaxFact::new(
                    facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
                    marker.parent(),
                    marker.kind(),
                    marker.span(),
                    marker.depth(),
                    marker.syntax_kind().clone(),
                ));
            } else {
                facts.retain(|fact| fact.local_id() != marker.local_id());
            }
            let mock = MockParseProvider::new(
                provider.capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let analyzer = TreeSitterAnalyzer::new(
                Arc::new(mock),
                ProducerIdentity::new("php-ordinary-replay", "1.0", content_hash(b"php-replay"))
                    .unwrap(),
                LanguageId::new("php").unwrap(),
                "php-ordinary-replay",
                content_hash(b"php-replay"),
            )
            .unwrap();
            let output = execute_analysis(
                &analyzer,
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            defined(&output, EntityKind::Parameter, 358, 362);
            let site = output
                .document()
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "php.ordinary_formal.reference"
                        && site.source.span().start_byte() == 440
                })
                .unwrap();
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        }
        Ok(())
    })
    .unwrap();
}

#[test]
fn legacy_variable_relabelling_and_counterfeit_scope_or_header_cannot_bypass_raw_proof() {
    use rootlight_adapter_sdk::{
        ParseProvider, SyntaxFact, SyntaxKindLabel, execute_parse, testkit::MockParseProvider,
    };
    analyze_observed(ORIGINAL.as_bytes(), "utf-8", |_, provider, request| {
        let cancel = Cancellation::new();
        let parsed = execute_parse(
            provider,
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel,
        )?;
        let replay = |facts| {
            let mock = MockParseProvider::new(
                provider.capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let analyzer = TreeSitterAnalyzer::new(
                Arc::new(mock),
                ProducerIdentity::new(
                    "php-ordinary-raw-proof",
                    "1.0",
                    content_hash(b"php-raw-proof"),
                )
                .unwrap(),
                LanguageId::new("php").unwrap(),
                "php-ordinary-raw-proof",
                content_hash(b"php-raw-proof"),
            )
            .unwrap();
            execute_analysis(
                &analyzer,
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
        };
        original_obligations(&replay(parsed.facts().to_vec())?);
        let labels = [
            "php.ordinary_clean_file.signature",
            "php.ordinary_function_context.signature",
            "php.parameters.signature",
            "php.ordinary_parameter.declaration",
            "php.variable_name.definition",
            "php.ordinary_call_target.signature",
            "php.ordinary_call_context.signature",
            "php.block.scope",
        ];
        for label in labels {
            let witness = parsed
                .facts()
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == label)
                .unwrap();
            for mode in 0..3 {
                let mut facts = parsed.facts().to_vec();
                if mode == 0 {
                    facts.retain(|fact| fact.local_id() != witness.local_id());
                    for fact in &mut facts {
                        if fact.parent() == Some(witness.local_id()) {
                            *fact = SyntaxFact::new(
                                fact.local_id(),
                                witness.parent(),
                                fact.kind(),
                                fact.span(),
                                fact.depth(),
                                fact.syntax_kind().clone(),
                            );
                        }
                    }
                } else if mode == 1 {
                    facts.push(SyntaxFact::new(
                        facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
                        witness.parent(),
                        witness.kind(),
                        witness.span(),
                        witness.depth(),
                        witness.syntax_kind().clone(),
                    ));
                } else {
                    let fact = facts
                        .iter_mut()
                        .find(|fact| fact.local_id() == witness.local_id())
                        .unwrap();
                    *fact = SyntaxFact::new(
                        fact.local_id(),
                        fact.parent(),
                        fact.kind(),
                        fact.span(),
                        fact.depth() + 1,
                        fact.syntax_kind().clone(),
                    );
                }
                if let Ok(output) = replay(facts) {
                    if label == "php.ordinary_call_target.signature"
                        || label == "php.ordinary_call_context.signature"
                    {
                        assert!(
                            !output
                                .document()
                                .relations
                                .iter()
                                .any(|edge| edge.predicate == RelationPredicate::Calls),
                            "{label} mode {mode}"
                        );
                    } else {
                        assert!(
                            !output
                                .document()
                                .occurrences
                                .iter()
                                .any(|site| site.syntax_kind == "php.ordinary_formal.reference"
                                    && site.source.span().start_byte() == 440
                                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
                            "{label} mode {mode}"
                        );
                    }
                }
            }
        }
        for start in [358, 440] {
            let mut facts = parsed.facts().to_vec();
            let fact = facts
                .iter_mut()
                .find(|fact| {
                    fact.span().start_byte() == start
                        && matches!(
                            fact.syntax_kind().as_str(),
                            "php.ordinary_formal.reference"
                                | "php.ordinary_unproved_variable.reference"
                        )
                })
                .unwrap();
            *fact = SyntaxFact::new(
                fact.local_id(),
                fact.parent(),
                fact.kind(),
                fact.span(),
                fact.depth(),
                SyntaxKindLabel::new("php.variable_name.reference").unwrap(),
            );
            assert!(
                replay(facts).is_err(),
                "a legacy label cannot escape the ordinary frame"
            );
        }
        Ok(())
    })
    .unwrap();
}

#[test]
fn source_mutation_is_rejected_by_artifact_identity_before_a_stale_native_certificate_is_used() {
    analyze_observed(ORIGINAL.as_bytes(), "utf-8", |analyzer, _, request| {
        let artifact = analyzer.prepare_artifact(request, &Cancellation::new())?;
        let changed = ORIGINAL.replacen("return fallbackBox", "return fallbackFoo", 1);
        analyze_observed(changed.as_bytes(), "utf-8", |_, _, changed_request| {
            assert!(
                analyzer
                    .analyze_from_artifact(
                        changed_request,
                        &artifact,
                        ExtensionSupport::default(),
                        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                        &Cancellation::new()
                    )
                    .is_err()
            );
            Ok(())
        })?;
        Ok(())
    })
    .unwrap();
}

#[test]
fn added_call_occurrence_relation_and_string_reservations_refuse_one_less_on_both_routes() {
    analyze_observed(ORIGINAL.as_bytes(), "utf-8", |analyzer, _, request| {
        let cancel = Cancellation::new();
        for field in ["occurrences", "relations", "strings"] {
            let for_capacity = |capacity| {
                let mut ir = request.limits().ir().clone();
                match field {
                    "occurrences" => ir.max_occurrences = capacity,
                    "relations" => ir.max_relations = capacity,
                    _ => ir.max_total_string_bytes = capacity,
                }
                AnalysisLimits::new(
                    request.limits().max_source_bytes(),
                    request.limits().max_syntax_nodes(),
                    request.limits().max_syntax_depth(),
                    request.limits().max_embedded_ranges(),
                    request.limits().max_reported_memory_bytes(),
                    request.limits().syntax_stream().clone(),
                    request.limits().ir_stream().clone(),
                    ir,
                )
                .unwrap()
            };
            let trial = |capacity, direct| {
                let limits = for_capacity(capacity);
                let selected = AnalysisRequest::new_with_parse_context(
                    request.source().clone(),
                    request.language().clone(),
                    request.encoding().clone(),
                    Vec::new(),
                    AnalysisTier::TierD,
                    request.build_context(),
                    &limits,
                )
                .unwrap()
                .with_generated_status(false);
                if direct {
                    execute_analysis(
                        analyzer,
                        &selected,
                        ExtensionSupport::default(),
                        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                        &cancel,
                    )
                } else {
                    analyzer
                        .prepare_artifact(&selected, &cancel)
                        .and_then(|artifact| {
                            analyzer.analyze_from_artifact(
                                &selected,
                                &artifact,
                                ExtensionSupport::default(),
                                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                                &cancel,
                            )
                        })
                }
            };
            let mut high = match field {
                "occurrences" => request.limits().ir().max_occurrences,
                "relations" => request.limits().ir().max_relations,
                _ => request.limits().ir().max_total_string_bytes,
            };
            original_obligations(&trial(high, false)?);
            let mut low = 1usize;
            while low < high {
                let middle = low + (high - low) / 2;
                if trial(middle, false).is_ok() {
                    high = middle;
                } else {
                    low = middle + 1;
                }
            }
            assert!(low > 1);
            let exact = trial(low, false)?;
            let native = trial(low, true)?;
            original_obligations(&exact);
            assert_eq!(native.document(), exact.document());
            for direct in [false, true] {
                let error = trial(low - 1, direct).unwrap_err();
                assert!(
                    matches!(
                        error,
                        rootlight_adapter_sdk::AdapterError::Sink(
                            rootlight_adapter_sdk::SinkError::StreamLimit { .. }
                        )
                    ),
                    "{field}: {error:?}"
                );
            }
        }
        Ok(())
    })
    .unwrap();
}

#[test]
fn explicit_whole_file_included_ranges_and_mixed_templates_do_not_admit_ordinary_parameters() {
    analyze_observed(ORIGINAL.as_bytes(), "utf-8", |analyzer, _, request| {
        let selected = AnalysisRequest::new_with_parse_context(
            request.source().clone(),
            request.language().clone(),
            request.encoding().clone(),
            vec![rootlight_adapter_sdk::IncludedRange::new(
                request.source().source_ref().span(),
                request.language().clone(),
            )],
            AnalysisTier::TierD,
            request.build_context(),
            request.limits(),
        )
        .unwrap()
        .with_generated_status(false);
        let output = execute_analysis(
            analyzer,
            &selected,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )?;
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Parameter)
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
        Ok(())
    })
    .unwrap();
    let template = analyze("<b>heading</b><?php function owned($lat) { return $lat; }");
    assert!(
        !template
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Parameter)
    );
}

#[test]
fn ordinary_functions_agree_at_exact_native_artifact_capacity_and_one_less() {
    analyze_observed(
        ORIGINAL.as_bytes(),
        "utf-8",
        |analyzer, provider, request| {
            let cancel = Cancellation::new();
            let required =
                provider.required_syntax_fact_count(&request.to_parse_request(), &cancel)?;
            let artifact = analyzer.prepare_artifact(request, &cancel)?;
            assert_eq!(artifact.required_syntax_fact_count(&cancel)?, required);
            let syntax = request.limits().syntax_stream();
            let for_capacity = |capacity| {
                AnalysisLimits::new(
                    request.limits().max_source_bytes(),
                    request.limits().max_syntax_nodes(),
                    request.limits().max_syntax_depth(),
                    request.limits().max_embedded_ranges(),
                    request.limits().max_reported_memory_bytes(),
                    StreamLimits::new(
                        syntax.max_batches(),
                        capacity,
                        syntax.max_output_bytes(),
                        syntax.max_diagnostics(),
                        syntax.max_diagnostic_bytes(),
                        syntax.max_string_bytes(),
                        BatchThresholds::new(
                            capacity.min(syntax.batch().max_records()),
                            syntax.batch().max_output_bytes(),
                            syntax.batch().max_diagnostics(),
                            syntax.batch().max_diagnostic_bytes(),
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                    request.limits().ir_stream().clone(),
                    request.limits().ir().clone(),
                )
                .unwrap()
            };
            let exact_limits = for_capacity(required);
            let exact = AnalysisRequest::new_with_parse_context(
                request.source().clone(),
                request.language().clone(),
                request.encoding().clone(),
                Vec::new(),
                AnalysisTier::TierD,
                request.build_context(),
                &exact_limits,
            )
            .unwrap()
            .with_generated_status(false);
            let exact_artifact = analyzer.prepare_artifact(&exact, &cancel)?;
            assert_eq!(
                exact_artifact.required_syntax_fact_count(&cancel)?,
                required
            );
            assert_eq!(exact_artifact.syntax_fact_count(), required);
            let output = execute_analysis(
                analyzer,
                &exact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            let replay = analyzer.analyze_from_artifact(
                &exact,
                &exact_artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            assert_eq!(output.document(), replay.document());
            original_obligations(&output);
            let small_limits = for_capacity(required - 1);
            let small = AnalysisRequest::new_with_parse_context(
                request.source().clone(),
                request.language().clone(),
                request.encoding().clone(),
                Vec::new(),
                AnalysisTier::TierD,
                request.build_context(),
                &small_limits,
            )
            .unwrap()
            .with_generated_status(false);
            let expected = rootlight_adapter_sdk::AdapterError::Sink(
                rootlight_adapter_sdk::SinkError::StreamLimit {
                    resource: rootlight_adapter_sdk::ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1,
                },
            );
            assert_eq!(
                analyzer.prepare_artifact(&small, &cancel).unwrap_err(),
                expected
            );
            // Preflight measures demand independently of output admission.
            assert_eq!(
                provider.required_syntax_fact_count(&small.to_parse_request(), &cancel)?,
                required
            );
            assert_eq!(
                rootlight_adapter_sdk::execute_parse(
                    provider,
                    &small.to_parse_request(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancel,
                )
                .unwrap_err(),
                expected
            );
            Ok(())
        },
    )
    .unwrap();
}
