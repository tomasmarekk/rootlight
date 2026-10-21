//! Coq native declarations retain authored identities and original byte ranges.
//! These controls deliberately leave kernel checking and flat module owners open.

use super::*;
use rootlight_ir::ContainerRef;

fn certificate_output(source: &str) -> AnalysisOutput {
    let fixture = Fixture::new(COQ, source.as_bytes());
    let provider = Arc::new(provider());
    let limits = limits();
    let result = analyze(
        &analyzer(&provider, COQ),
        &request(&fixture.snapshot, &fixture.source, COQ, &limits),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(
                |site| site.source.repository() == fixture.source.repository()
                    && site.source.generation() == fixture.source.generation()
                    && site.source.content_hash() == fixture.source.content_hash()
            )
    );
    result
}

#[test]
fn coq_parameterized_assertions_preserve_exact_names_and_immediate_formal_owners() {
    for alias in [
        "Theorem",
        "Lemma",
        "Fact",
        "Remark",
        "Corollary",
        "Proposition",
        "Property",
    ] {
        let source = format!("{alias} written {{A B}} (x y : A) (z : B) : True.\n");
        let result = certificate_output(&source);
        let document = result.document();
        let function = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Function && entity.canonical_name == "written"
            })
            .unwrap();
        named_definition(document, "written", EntityKind::Function);
        let end = u64::try_from(source.find(": True").unwrap() + 1).unwrap();
        assert_eq!(
            function.evidence.source.as_ref().unwrap().span().end_byte(),
            end
        );
        for name in ["x", "y", "z"] {
            let definition = named_definition(document, name, EntityKind::Parameter);
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("formal target")
            };
            let parameter = document
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap();
            assert_eq!(parameter.container, Some(ContainerRef::Entity(function.id)));
            assert!(
                document
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::Contains
                        && relation.subject == RelationEndpoint::Entity(function.id)
                        && relation.object == RelationEndpoint::Entity(symbol))
            );
            let span = definition.source.span();
            assert_eq!(
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                name
            );
        }
        for name in ["A", "B"] {
            assert!(
                !document
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == name)
            );
        }
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "coq-written-formal-types-and-term-lifetime-unavailable")
        );
    }
}

#[test]
fn coq_parameterized_header_barriers_never_publish_a_constant_fallback() {
    for source in [
        "Lemma blocked (x x : nat) : True.",
        "Lemma blocked {x} (x : nat) : True.",
        "Lemma blocked (x := 0) : True.",
        "Lemma blocked x : True.",
        "Module M. Lemma blocked (x : nat) : True.",
        "Section S. Lemma blocked (x : nat) : True.",
        "Lemma pending : True. Lemma blocked (x : nat) : True.",
        "Fail Lemma blocked (x : nat) : True.",
        "#[local] Lemma blocked (x : nat) : True.",
    ] {
        let result = certificate_output(source);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| matches!(entity.canonical_name.as_str(), "blocked" | "x")),
            "{source}"
        );
        assert!(!result.document().skipped_regions.is_empty());
    }
    let source = "Theorem zero : True. Proof. exact I. Qed.\nDefinition constant := 0.\nFixpoint recursive (x : nat) : nat := x.\n";
    let result = certificate_output(source);
    for name in ["zero", "constant", "recursive"] {
        named_definition(result.document(), name, EntityKind::Constant);
    }
}

#[test]
fn coq_notation_pair_preserves_record_constructor_and_field_identity_through_normalization() {
    let prefix = "Record target (A : Type) := make { field : A }.\n";
    let source = format!("{prefix}Notation \"! T\" := (target T) (at level 10).\n");
    let before = certificate_output(prefix);
    let after = certificate_output(&source);
    let document = after.document();
    for (name, kind) in [
        ("target", EntityKind::Struct),
        ("make", EntityKind::Constructor),
        ("field", EntityKind::Field),
    ] {
        let old = before
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap();
        let new = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap();
        assert_eq!(old.id, new.id, "{name}");
        assert_eq!(old.container, new.container);
    }
    let target = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "target")
        .unwrap();
    let start = u64::try_from(source.rfind("target").unwrap()).unwrap();
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().start_byte() == start && site.role == OccurrenceRole::Reference
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert!(
        document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
                && edge.object == RelationEndpoint::Entity(target.id))
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.source.span().start_byte() == start
                && site.role == OccurrenceRole::Definition)
    );
    let fixture = Fixture::new(COQ, source.as_bytes());
    let provider = Arc::new(provider());
    let limits = limits();
    let parsed = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &request(&fixture.snapshot, &fixture.source, COQ, &limits).to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let markers: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "coq.notation_target.signature")
        .collect();
    assert_eq!(markers.len(), 1);
    assert_eq!(
        markers[0].kind(),
        rootlight_adapter_sdk::SyntaxFactKind::Signature
    );
    assert_eq!(markers[0].span().start_byte(), 0);
    assert_eq!(markers[0].span().end_byte(), start + 6);
}

#[test]
fn coq_unsupported_notation_contexts_retain_unresolved_reference_and_specific_gaps() {
    for middle in [
        "Definition target := 1. ",
        "Import Other. ",
        "Require Import Other. ",
        "Export Other. ",
        "Include Other. ",
        "Section S. ",
        "Module M. ",
        "Set Implicit Arguments. ",
    ] {
        let source = format!("Definition target := 0. {middle}Notation \"! T\" := (target T).\n");
        let result = certificate_output(&source);
        let start = u64::try_from(source.rfind("target").unwrap()).unwrap();
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == start && site.role == OccurrenceRole::Reference
            })
            .collect();
        assert_eq!(sites.len(), 1, "{source}");
        assert!(
            matches!(sites[0].target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "coq-declaration-time-notation-binding-unavailable")
        );
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo)
        );
    }
    for pattern in ["target T", "'target' T"] {
        let source = format!("Definition target := 0. Notation \"{pattern}\" := (target T).\n");
        let result = certificate_output(&source);
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo)
        );
    }
}

#[test]
fn coq_notation_artifacts_keep_current_generation_and_exact_native_required_demand() {
    let source = "Definition target := 0. Notation \"! T\" := (target T).\nLemma written (x : nat) : ∥ x ∥.\n";
    let fixture = Fixture::new(COQ, source.as_bytes());
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, COQ);
    let limits = limits();
    let initial = request(&fixture.snapshot, &fixture.source, COQ, &limits);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let required = artifact.required_syntax_fact_count(&deadline()).unwrap();
    assert_eq!(
        required,
        provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap()
    );
    let successor = fixture.next_generation();
    let next = request(&successor.snapshot, &successor.source, COQ, &limits);
    let reused = analyzer
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let clean = analyze(&analyzer, &next, &ExtensionSupport::default());
    assert_eq!(reused.document(), clean.document());
    assert_eq!(reused.report(), clean.report());
    assert_eq!(symbol_ids(first.document()), symbol_ids(reused.document()));
    assert!(
        reused
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.generation() == successor.source.generation())
    );
    assert!(
        reused
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo)
    );
    let exact_limits = limits_with_syntax_records(required);
    let exact = request(&successor.snapshot, &successor.source, COQ, &exact_limits);
    let exact_output = analyze(&analyzer, &exact, &ExtensionSupport::default());
    assert!(
        exact_output
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo)
    );
    let too_small = limits_with_syntax_records(required - 1);
    assert!(matches!(
        analyzer.analyze_and_capture(
            &request(&successor.snapshot, &successor.source, COQ, &too_small),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline()
        ),
        Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            ..
        }))
    ));
}

#[test]
fn coq_unicode_bom_and_crlf_keep_physical_formals_and_explicit_utf16_binding_gap() {
    let text = "(* λ😀 *)\r\nRecord résumé (A : Type) := crée { valeur : A }.\r\nNotation \"! T\" := (résumé T).\r\nLemma écrit (paramètre : nat) : True.\r\n";
    for (encoding, bom) in [
        ("utf-8", false),
        ("utf-8", true),
        ("utf-16le", true),
        ("utf-16be", true),
    ] {
        let text = format!("{}{text}", if bom { "\u{feff}" } else { "" });
        let encode = |part: &str| -> Vec<u8> {
            if encoding == "utf-8" {
                part.as_bytes().to_vec()
            } else {
                part.encode_utf16()
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
        let raw = encode(&text);
        let fixture = Fixture::new(COQ, &raw);
        let provider = Arc::new(provider());
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(COQ),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, COQ),
            &request,
            &ExtensionSupport::default(),
        );
        validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        for (name, kind) in [
            ("résumé", EntityKind::Struct),
            ("écrit", EntityKind::Function),
            ("paramètre", EntityKind::Parameter),
        ] {
            let site = named_definition(result.document(), name, kind);
            let start = encode(&text[..text.find(name).unwrap()]).len();
            assert_eq!(
                site.source.span().start_byte(),
                u64::try_from(start).unwrap()
            );
            assert_eq!(
                site.source.span().end_byte(),
                u64::try_from(start + encode(name).len()).unwrap()
            );
            assert_eq!(site.source.content_hash(), content_hash(&raw));
        }
        let use_start =
            u64::try_from(encode(&text[..text.rfind("résumé").unwrap()]).len()).unwrap();
        let uses: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == use_start
                    && site.role == OccurrenceRole::Reference
            })
            .collect();
        assert_eq!(uses.len(), 1, "{encoding}/{bom}");
        assert_eq!(
            matches!(uses[0].target, OccurrenceTarget::Resolved { .. }),
            encoding == "utf-8"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Relations)
        );
    }
}

#[test]
fn coq_separately_included_ranges_do_not_claim_a_standalone_prefix_certificate() {
    let source = "(* omitted *)\nDefinition target := 0. Notation \"! T\" := (target T). Lemma written (x : nat) : True.";
    let fixture = Fixture::new(COQ, source.as_bytes());
    let provider = Arc::new(provider());
    let budget = limits();
    let begin = u64::try_from(source.find("Definition").unwrap()).unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(COQ),
        EncodingId::utf8(),
        vec![IncludedRange::new(
            SourceSpan::new(
                fixture.source.span().file(),
                begin,
                u64::try_from(source.len()).unwrap(),
            )
            .unwrap(),
            language(COQ),
        )],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, COQ),
        &request,
        &ExtensionSupport::default(),
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| matches!(entity.canonical_name.as_str(), "written" | "x"))
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo)
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "coq-declaration-time-notation-binding-unavailable")
    );
}

#[test]
fn coq_prepared_certificates_reject_changed_context_content_and_cancelled_staging() {
    let source = "Definition target := 0. Notation \"! T\" := (target T).\n";
    let fixture = Fixture::new(COQ, source.as_bytes());
    let (analyzer, parser) = counting_analyzer(COQ, ParseTestBehavior::Native);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, COQ, &budget);
    let artifact = analyzer.prepare_artifact(&initial, &deadline()).unwrap();
    let changed = fixture.rewrite(source.replace(":= 0", ":= 1").as_bytes());
    let changed_request = request(&changed.snapshot, &changed.source, COQ, &budget);
    let included = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(COQ),
        EncodingId::utf8(),
        vec![IncludedRange::new(fixture.source.span(), language(COQ))],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    for invalid in [&changed_request, &included] {
        assert!(
            matches!(analyzer.analyze_prepared_artifact(invalid, &artifact, ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()),
            Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
        );
    }
    let cancelled = deadline();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyzer.analyze_prepared_artifact(
            &initial,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert_eq!(parser.invocations.load(Ordering::Relaxed), 1);
    let (after_parse, _) = counting_analyzer(COQ, ParseTestBehavior::CancelAfterParse);
    assert!(matches!(
        after_parse.analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline()
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn coq_generated_classification_reuses_parse_and_rebuilds_file_metadata() {
    let source = "Definition target := 0. Notation \"! T\" := (target T).\n";
    let fixture = Fixture::new(COQ, source.as_bytes());
    let (analyzer, parser) = counting_analyzer(COQ, ParseTestBehavior::Native);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, COQ, &budget);
    let artifact = analyzer.prepare_artifact(&initial, &deadline()).unwrap();
    let classified =
        request(&fixture.snapshot, &fixture.source, COQ, &budget).with_generated_status(true);
    let replayed = analyzer
        .analyze_prepared_artifact(
            &classified,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(parser.invocations.load(Ordering::Relaxed), 1);
    assert!(replayed.document().files[0].generated);
    let fresh = analyze(&analyzer, &classified, &ExtensionSupport::default());
    assert_eq!(replayed.document(), fresh.document());
    assert_eq!(parser.invocations.load(Ordering::Relaxed), 2);
}

const COQ: LanguageCase = LanguageCase {
    name: "coq",
    path: "src/written.v",
    frontend: "tree-sitter-rocq-0.2.0",
    source: "Definition value := 0.\n",
    generated: false,
    body_before: "0",
    body_after: "1",
};

fn record_field<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    name: &str,
) -> &'a rootlight_ir::EntityRecord {
    let fields: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Field && entity.canonical_name == name)
        .collect();
    assert_eq!(fields.len(), 1, "{name}: {:?}", document.entities);
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.target
                == OccurrenceTarget::Resolved {
                    symbol: fields[0].id,
                }
        })
        .collect();
    assert_eq!(sites.len(), 1, "{name}");
    assert_eq!(sites[0].role, OccurrenceRole::Declaration);
    fields[0]
}

#[test]
fn coq_record_class_original_fields_and_constructors_keep_physical_owners() {
    for (source, name, kind, constructor, fields) in [
        (
            include_str!("../fixtures/coq-records/binder_annot.v"),
            "binder_annot",
            EntityKind::Struct,
            Some("mkBindAnn"),
            vec!["binder_name", "binder_relevance"],
        ),
        (
            include_str!("../fixtures/coq-records/checker_flags.v"),
            "checker_flags",
            EntityKind::Class,
            None,
            vec![
                "check_univs",
                "prop_sub_type",
                "indices_matter",
                "lets_in_constructor_types",
            ],
        ),
        (
            include_str!("../fixtures/coq-records/retroknowledge.v"),
            "t",
            EntityKind::Struct,
            Some("mk_retroknowledge"),
            vec![
                "retro_int63",
                "retro_float64",
                "retro_string",
                "retro_array",
            ],
        ),
        (
            include_str!("../fixtures/coq-records/listable.v"),
            "Listable",
            EntityKind::Class,
            None,
            vec!["list_all", "find_index", "cardinality"],
        ),
        (
            include_str!("../fixtures/coq-records/dependent_sigma.v"),
            "Sigma",
            EntityKind::Struct,
            None,
            vec!["fst", "snd"],
        ),
    ] {
        let result = output(source);
        let document = result.document();
        named_definition(document, name, kind);
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.kind == kind && entity.canonical_name == name)
            .unwrap();
        for field in fields {
            let member = record_field(document, field);
            assert_eq!(
                member.container,
                Some(ContainerRef::Entity(owner.id)),
                "{field}"
            );
            assert!(
                document
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(owner.id)
                        && edge.object == RelationEndpoint::Entity(member.id))
            );
        }
        let constructors: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Constructor)
            .collect();
        assert_eq!(constructors.len(), usize::from(constructor.is_some()));
        if let Some(name) = constructor {
            named_definition(document, name, EntityKind::Constructor);
            assert_eq!(
                constructors[0].container,
                Some(ContainerRef::Entity(owner.id))
            );
        }
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "check_guard"
                    || entity.canonical_name.starts_with("Build_"))
        );
        assert!(document.skipped_regions.iter().any(|gap| gap.detail
            == "coq-record-class-elaboration-projection-generation-and-field-binding-unavailable"));
    }
}

#[test]
fn coq_record_class_singleton_and_structure_do_not_invent_generated_entities() {
    let singleton = include_str!("../fixtures/coq-records/debug_opt.v");
    let result = output(singleton);
    named_definition(result.document(), "debug_opt", EntityKind::Class);
    record_field(result.document(), "debug");
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Constructor)
    );
    let source = "Structure Pair := make_pair { left : nat; right : nat }.\nRecord Anonymous := { _ : nat; _value : nat }.\nDefinition made := {| left := 0; right := 1 |}.\n";
    let result = output(source);
    named_definition(result.document(), "Pair", EntityKind::Struct);
    named_definition(result.document(), "make_pair", EntityKind::Constructor);
    named_definition(result.document(), "Anonymous", EntityKind::Struct);
    for name in ["left", "right", "_value"] {
        record_field(result.document(), name);
    }
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "_"
                || entity.canonical_name.starts_with("Build_"))
    );
}

#[test]
fn coq_record_class_attributes_recovery_and_decoys_keep_applicable_gaps() {
    let source = "(* Record comment_fake := { fake : nat }. *)\nDefinition text := \"Class string_fake := { fake : nat }.\".\nFail Record failed := { failed_field : nat }.\n#[local] Class attributed := { attributed_field : nat }.\nRecord nested := { #[canonical] nested_field : nat }.\nRecord _ := { anonymous_head_field : nat }.\nRecord broken := { broken_field : }.\nDefinition tail := 0.\n";
    let result = output(source);
    named_definition(result.document(), "tail", EntityKind::Constant);
    for name in [
        "comment_fake",
        "string_fake",
        "fake",
        "failed",
        "failed_field",
        "attributed",
        "attributed_field",
        "nested",
        "nested_field",
        "_",
        "anonymous_head_field",
        "broken",
        "broken_field",
    ] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "{name}"
        );
    }
    assert!(
        result.document().skipped_regions.iter().any(
            |gap| gap.detail == "coq-fail-attribute-or-selector-declaration-effect-unavailable"
        )
    );
    let attributed = output(include_str!("../fixtures/coq-records/sigma.v"));
    assert!(
        !attributed.document().entities.iter().any(|entity| matches!(
            entity.kind,
            EntityKind::Struct | EntityKind::Field | EntityKind::Constructor
        ))
    );
    assert!(
        attributed.document().skipped_regions.iter().any(
            |gap| gap.detail == "coq-fail-attribute-or-selector-declaration-effect-unavailable"
        )
    );
}

#[test]
fn coq_record_class_encoded_names_keep_original_sources_and_unresolved_values() {
    let text = "(* λ😀 *)\nRecord résumé := crée { valeur : nat := source_value }.\nClass unique := projection : nat.\n";
    for (encoding, bom) in [
        ("utf-8", false),
        ("utf-8", true),
        ("utf-16le", true),
        ("utf-16be", true),
    ] {
        let source = format!("{}{text}", if bom { "\u{feff}" } else { "" });
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
        let raw = encode(&source);
        let provider = Arc::new(provider());
        let fixture = Fixture::new(COQ, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(COQ),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, COQ),
            &request,
            &ExtensionSupport::default(),
        );
        validate(&result, &raw, encoding);
        for (name, kind, role) in [
            ("résumé", EntityKind::Struct, OccurrenceRole::Definition),
            ("crée", EntityKind::Constructor, OccurrenceRole::Definition),
            ("valeur", EntityKind::Field, OccurrenceRole::Declaration),
            ("unique", EntityKind::Class, OccurrenceRole::Definition),
            ("projection", EntityKind::Field, OccurrenceRole::Declaration),
        ] {
            let entity = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name && entity.kind == kind)
                .unwrap();
            let sites: Vec<_> = result
                .document()
                .occurrences
                .iter()
                .filter(|site| site.target == OccurrenceTarget::Resolved { symbol: entity.id })
                .collect();
            assert_eq!(sites.len(), 1, "{encoding}/{bom}: {name}");
            let site = sites[0];
            let start = encode(&source[..source.find(name).unwrap()]).len();
            assert_eq!(site.role, role);
            assert_eq!(
                site.source.span().start_byte(),
                u64::try_from(start).unwrap()
            );
            assert_eq!(
                site.source.span().end_byte(),
                u64::try_from(start + encode(name).len()).unwrap()
            );
            assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
            assert_eq!(
                entity.evidence.source.as_ref().unwrap().content_hash(),
                content_hash(&raw)
            );
            assert_eq!(
                result
                    .document()
                    .provenance
                    .iter()
                    .find(|record| record.id == entity.provenance)
                    .unwrap()
                    .tier,
                AnalysisTier::TierD
            );
        }
        let start = encode(&source[..source.find("source_value").unwrap()]).len();
        let uses: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.role == OccurrenceRole::Reference
            })
            .collect();
        assert_eq!(uses.len(), 1, "{encoding}/{bom}");
        assert!(matches!(
            uses[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert_eq!(
            uses[0].source.span().end_byte(),
            u64::try_from(start + encode("source_value").len()).unwrap()
        );
    }
}

#[test]
fn coq_record_class_custom_types_and_field_dependencies_keep_explicit_uncertainty() {
    let source = "Definition chosen := 0.\nRecord Values := { chosen_field : nat; value : nat := chosen_field; method (input : nat) : nat := input }.\nRecord Ambiguous := { unknown_field : nat | 3 }.\nClass Ranked := { ranked : C | 2; default := chosen }.\n";
    let result = output(source);
    let document = result.document();
    for name in [
        "chosen_field",
        "value",
        "method",
        "unknown_field",
        "ranked",
        "default",
    ] {
        record_field(document, name);
    }
    for spelling in ["chosen_field", "chosen", "input"] {
        let at = source.rfind(spelling).unwrap();
        let uses: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == u64::try_from(at).unwrap()
            })
            .collect();
        assert_eq!(uses.len(), 1, "{spelling}");
        assert!(matches!(
            uses[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
    let start = source.find("unknown_field").unwrap();
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "coq-custom-notation-or-generic-tactic-structure-unavailable"
        && gap.source.span().start_byte() == u64::try_from(start).unwrap()));
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "coq-local-term-binder-projection-and-generated-declaration-coverage-incomplete"));
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "input")
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name.starts_with("Build_"))
    );
}

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    output_with_provider(source, &provider)
}

fn output_with_provider(source: &str, provider: &Arc<TreeSitterProvider>) -> AnalysisOutput {
    let fixture = Fixture::new(COQ, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(provider, COQ),
        &request(&fixture.snapshot, &fixture.source, COQ, &budget),
        &ExtensionSupport::default(),
    );
    validate(&result, source.as_bytes(), "utf-8");
    result
}

fn validate(result: &AnalysisOutput, source: &[u8], encoding: &str) {
    let document = result.document();
    validate_ir_document(document, limits().ir(), &ExtensionSupport::default()).unwrap();
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "coq-elaboration-typechecking-proofchecking-and-term-binding-unavailable"));
    for occurrence in &document.occurrences {
        let span = occurrence.source.span();
        let bytes = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        let spelling = if encoding == "utf-8" {
            std::str::from_utf8(bytes).unwrap().to_owned()
        } else {
            assert_eq!(bytes.len() % 2, 0);
            String::from_utf16(
                &bytes
                    .chunks_exact(2)
                    .map(|unit| {
                        let unit = [unit[0], unit[1]];
                        if encoding == "utf-16le" {
                            u16::from_le_bytes(unit)
                        } else {
                            u16::from_be_bytes(unit)
                        }
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(spelling.as_bytes()),
            "{encoding}: {}",
            occurrence.syntax_kind
        );
        assert_eq!(occurrence.source.content_hash(), content_hash(source));
        assert_eq!(occurrence.source.generation(), document.generation);
        assert_eq!(occurrence.source.repository(), document.repository);
        assert_ne!(occurrence.role, OccurrenceRole::CallSite);
        if occurrence.role == OccurrenceRole::Declaration {
            assert!(matches!(
                occurrence.syntax_kind.as_str(),
                "coq.declared_instance_name.definition"
                    | "coq.record_field_name.definition"
                    | "coq.class_field_name.definition"
            ));
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { .. }
            ));
        } else if occurrence.role != OccurrenceRole::Definition {
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
    }
}

fn named_definition<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
) -> &'a rootlight_ir::OccurrenceRecord {
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == name && entity.kind == kind)
        .unwrap_or_else(|| {
            panic!(
                "missing {name}: {:?}; gaps {:?}",
                document.entities, document.skipped_regions
            )
        });
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
        })
        .collect();
    assert_eq!(definitions.len(), 1, "{name}");
    definitions[0]
}

#[test]
fn coq_core_names_and_inductive_constructor_owners_are_source_bound() {
    let source = "(* marker λ😀 *)\nDefinition résumé := 0.\nTheorem truth : True. Proof. exact I. Qed.\nFixpoint identity (x : nat) : nat := x.\nInductive flag := On | Off.\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("résumé", EntityKind::Constant),
        ("truth", EntityKind::Constant),
        ("identity", EntityKind::Constant),
        ("flag", EntityKind::Enum),
        ("On", EntityKind::Constructor),
        ("Off", EntityKind::Constructor),
    ] {
        let occurrence = named_definition(document, name, kind);
        let span = occurrence.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
    }
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "flag")
        .unwrap();
    for name in ["On", "Off"] {
        let child = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner.id)
                    && relation.object == RelationEndpoint::Entity(child.id))
        );
    }
    assert!(
        document
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Reference
                && occurrence.syntax_kind == "coq.written_reference.reference")
    );
}

#[test]
fn coq_fail_attributes_and_literal_decoys_never_become_global_definitions() {
    let source = "(* Definition comment_fake := 0. (* nested *) *)\nDefinition text := \"Definition string_fake := 0.\".\nFail Definition failed := 0.\n#[local] Definition attributed := 0.\nContext (A : Type).\nDefinition tail := 0.\n";
    let result = output(source);
    named_definition(result.document(), "text", EntityKind::Constant);
    named_definition(result.document(), "tail", EntityKind::Constant);
    for name in ["comment_fake", "string_fake", "failed", "attributed", "A"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "invented {name}"
        );
    }
    for gap in [
        "coq-fail-attribute-or-selector-declaration-effect-unavailable",
        "coq-context-and-assumption-binder-lifetime-unavailable",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|region| region.detail == gap)
        );
    }
}

#[test]
fn coq_flat_module_headers_do_not_invent_namespace_containment() {
    let source =
        "Module A.\nDefinition value := 0.\nEnd A.\nModule B.\nDefinition value := 1.\nEnd B.\n";
    let result = output(source);
    let document = result.document();
    for name in ["A", "B"] {
        named_definition(document, name, EntityKind::Module);
        let module = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.subject == RelationEndpoint::Entity(module.id))
        );
    }
    let values: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "value")
        .collect();
    assert_eq!(values.len(), 2);
    assert_ne!(values[0].id, values[1].id);
    assert!(document.skipped_regions.iter().any(|gap|gap.detail=="coq-module-section-stack-and-qualified-ownership-unavailable"));
}

#[test]
fn coq_recovery_and_generic_sentences_keep_explicit_applicable_gaps() {
    for source in [
        "Definition broken := .\n",
        "Context {term : Type@{i}}.\n",
        "MetaRocq Quote Definition quoted := nat.\n",
    ] {
        let result = output(source);
        assert!(
            result.document().skipped_regions.iter().any(|gap| matches!(
                gap.detail.as_str(),
                "coq-parser-recovery-structure-unavailable"
                    | "coq-custom-notation-or-generic-tactic-structure-unavailable"
                    | "coq-vernacular-construct-structure-unavailable"
                    | "coq-context-and-assumption-binder-lifetime-unavailable"
            )),
            "missing applicable gap for {source}: {:?}",
            result.document().skipped_regions
        );
    }
    let record = output("Record carrier := { field : nat }.\n");
    named_definition(record.document(), "carrier", EntityKind::Struct);
    record_field(record.document(), "field");
    assert!(record.document().skipped_regions.iter().any(|gap| gap.detail == "coq-record-class-elaboration-projection-generation-and-field-binding-unavailable"));
}

#[test]
fn coq_body_changes_preserve_written_constant_identity() {
    let before = output(COQ.source);
    let after = output("Definition value := 1.\n");
    let entity = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "value")
            .unwrap()
            .id
    };
    assert_eq!(entity(&before), entity(&after));
}

#[test]
fn coq_reserved_lone_underscore_is_not_a_declaration_name() {
    for name in ["_", "Type", "Definition", "match"] {
        let source = format!(
            "Definition {name} := 0.\nModule {name}.\nEnd {name}.\nDefinition _value := 0.\n"
        );
        let result = output(&source);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
        named_definition(result.document(), "_value", EntityKind::Constant);
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && gap.detail == "coq-declaration-name-unavailable")
        );
    }
}

#[test]
fn coq_term_and_functor_binders_are_explicit_entity_gaps_and_never_fake_reads() {
    let source = "Definition identity (x : nat) : nat := x.\nModule M (X : S).\nEnd M.\n";
    let result = output(source);
    let document = result.document();
    for name in ["x", "X"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == domain
                    && gap.detail
                        == "coq-term-and-module-binder-entities-and-lifetime-unavailable")
        );
    }
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect();
    let body_x = source.find(":= x").unwrap() + 3;
    assert!(
        references.iter().any(
            |occurrence| occurrence.source.span().start_byte() == body_x as u64
                && occurrence.source.span().end_byte() == body_x as u64 + 1
        )
    );
    for declaration_start in [
        source.find("identity").unwrap(),
        source.find("(x").unwrap() + 1,
        source.find("(X").unwrap() + 1,
        source.find("Module M").unwrap() + 7,
    ] {
        assert!(!references.iter().any(|occurrence| occurrence.source.span().start_byte() == declaration_start as u64));
    }
}

#[test]
fn coq_local_let_and_match_bindings_remain_explicitly_incomplete() {
    let source = "Definition local := let x := 0 in x.\nDefinition choose (n : nat) : nat := match n with | O => 0 | S y => y end.\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("local", EntityKind::Constant),
        ("choose", EntityKind::Function),
    ] {
        named_definition(document, name, kind);
    }
    for name in ["x", "y", "n"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
    assert!(document.skipped_regions.iter().any(|gap| gap.domain == FactDomain::Entities && gap.detail == "coq-local-term-binder-projection-and-generated-declaration-coverage-incomplete"));
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(
            result
                .report()
                .coverage()
                .domains()
                .iter()
                .any(|coverage| coverage.domain() == domain
                    && coverage.status() != CoverageStatus::Complete)
        );
    }
}

#[test]
fn coq_original_error_root_keeps_file_source_and_does_not_poison_clean_neighbors() {
    let source = include_str!("../fixtures/coq-recovery/MRListable.v");
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rocq::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert_eq!(tree.root_node().kind(), "ERROR");
    let provider = Arc::new(provider());
    let result = output_with_provider(source, &provider);
    let document = result.document();
    assert_eq!(document.files.len(), 1);
    assert_eq!(
        document.files[0].content_hash,
        content_hash(source.as_bytes())
    );
    assert_eq!(document.files[0].byte_length, source.len() as u64);
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "coq-parser-recovery-structure-unavailable")
    );
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.domain == domain
                && gap.detail == "coq-parser-recovery-structure-unavailable"
                && gap.source.span().start_byte() == 0
                && gap.source.span().end_byte() == source.len() as u64
        }));
    }
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    let neighbor = output_with_provider("Definition neighbor := 0.\n", &provider);
    named_definition(neighbor.document(), "neighbor", EntityKind::Constant);
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn coq_utf8_and_utf16_names_use_exact_original_physical_bytes() {
    let source = "\u{feff}(* λ😀 *)\r\nDefinition résumé := 0.\r\nInstance fourni : C := value.\r\nDeclare Instance requis : C.\r\n";
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
        let raw = encode(source);
        let provider = Arc::new(provider());
        let fixture = Fixture::new(COQ, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(COQ),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, COQ),
            &request,
            &ExtensionSupport::default(),
        );
        validate(&result, &raw, encoding);
        let occurrence = named_definition(result.document(), "résumé", EntityKind::Constant);
        let start = encode(&source[..source.find("résumé").unwrap()]).len();
        assert_eq!(
            occurrence.source.span().start_byte(),
            start as u64,
            "{encoding}"
        );
        assert_eq!(
            occurrence.source.span().end_byte(),
            (start + encode("résumé").len()) as u64,
            "{encoding}"
        );
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash("résumé".as_bytes())
        );
        for (name, role) in [
            ("fourni", OccurrenceRole::Definition),
            ("requis", OccurrenceRole::Declaration),
        ] {
            let entity = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name && entity.kind == EntityKind::Constant)
                .unwrap();
            let occurrence = result
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .unwrap();
            assert_eq!(occurrence.role, role);
            let start = encode(&source[..source.find(name).unwrap()]).len();
            assert_eq!(occurrence.source.span().start_byte(), start as u64);
            assert_eq!(
                occurrence.source.span().end_byte(),
                (start + encode(name).len()) as u64
            );
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(name.as_bytes())
            );
        }
    }
}

#[test]
fn coq_named_instances_and_declared_requirements_keep_distinct_written_roles() {
    let source = "(* marker λ😀 *)\nInstance provided (input : nat) : C := value.\nInstance methods : C := { field (item : nat) := body }.\nInstance pending : C.\nModule Type Contract.\nDeclare Instance required : C | 2.\nEnd Contract.\nExisting Instance provided.\nExisting Instances provided Module.prior.\nExisting Class C.\n";
    let result = output(source);
    let document = result.document();
    for name in ["provided", "methods", "pending"] {
        let occurrence = named_definition(document, name, EntityKind::Constant);
        let start = source.find(name).unwrap();
        assert_eq!(occurrence.source.span().start_byte(), start as u64);
        assert_eq!(
            occurrence.source.span().end_byte(),
            (start + name.len()) as u64
        );
    }
    let required = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Constant && entity.canonical_name == "required")
        .expect("written declared-instance requirement");
    let names: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.target
                == OccurrenceTarget::Resolved {
                    symbol: required.id,
                }
        })
        .collect();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].role, OccurrenceRole::Declaration);
    assert_eq!(
        names[0].source.span().start_byte(),
        source.find("required").unwrap() as u64
    );
    for name in ["C", "value", "body", "provided", "Module.prior"] {
        assert!(
            document.occurrences.iter().any(|occurrence| {
                occurrence.role == OccurrenceRole::Reference
                    && &source[usize::try_from(occurrence.source.span().start_byte()).unwrap()
                        ..usize::try_from(occurrence.source.span().end_byte()).unwrap()]
                        == name
                    && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            }),
            "missing unresolved source reference {name}"
        );
    }
    for name in [
        "input",
        "item",
        "field",
        "C",
        "value",
        "body",
        "Module.prior",
    ] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "invented declaration {name}"
        );
    }
    assert!(document.skipped_regions.iter().any(
        |gap| gap.detail == "coq-instance-typeclass-registration-and-field-binding-unavailable"
    ));
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "coq-module-section-stack-and-qualified-ownership-unavailable"));
}

#[test]
fn coq_original_unprefixed_instance_retains_its_exact_source_name() {
    let source = include_str!("../fixtures/coq-instances/subst_instance_list.v");
    assert_eq!(source.len(), 110);
    let result = output(source);
    let occurrence = named_definition(
        result.document(),
        "subst_instance_list",
        EntityKind::Constant,
    );
    assert_eq!(occurrence.source.span().start_byte(), 9);
    assert_eq!(occurrence.source.span().end_byte(), 28);
    assert!(result.document().skipped_regions.iter().any(|gap| gap.detail
        == "coq-term-and-module-binder-entities-and-lifetime-unavailable"));
}

#[test]
fn coq_instance_decoys_and_registrations_do_not_create_unwritten_declarations() {
    let source = "(* Instance comment_fake : C := value. *)\nDefinition text := \"Instance string_fake : C := value.\".\nFail Instance failed : C := value.\n#[export] Instance attributed : C := value.\nLocal Instance local_value : C := value.\n2: Instance selected : C := value.\nExisting Instance prior.\nExisting Class Carrier.\nInstance tail : C := @value.\nInstance methods : C := { field (item : nat) := @body }.\n";
    let result = output(source);
    named_definition(result.document(), "tail", EntityKind::Constant);
    named_definition(result.document(), "methods", EntityKind::Constant);
    for name in [
        "comment_fake",
        "string_fake",
        "failed",
        "attributed",
        "local_value",
        "selected",
        "prior",
        "Carrier",
        "field",
        "item",
    ] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "invented {name}"
        );
    }
    for name in ["value", "body", "prior", "Carrier"] {
        assert!(result.document().occurrences.iter().any(|occurrence| {
            occurrence.role == OccurrenceRole::Reference
                && &source[usize::try_from(occurrence.source.span().start_byte()).unwrap()
                    ..usize::try_from(occurrence.source.span().end_byte()).unwrap()]
                    == name
        }));
    }
    assert!(
        result.document().skipped_regions.iter().any(
            |gap| gap.detail == "coq-fail-attribute-or-selector-declaration-effect-unavailable"
        )
    );
    let malformed = output("Instance broken : C := { field := }.\n");
    let missing = malformed
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "syntax-error-recovery")
        .expect("the missing field value retains the native missing-node diagnostic");
    assert_eq!(missing.domain, FactDomain::Diagnostics);
    assert_eq!(missing.source.span().start_byte(), 33);
    assert_eq!(missing.source.span().end_byte(), 33);
    assert!(
        !malformed
            .document()
            .entities
            .iter()
            .any(|entity| { matches!(entity.canonical_name.as_str(), "broken" | "field") })
    );
    let recovered = output("Instance unclosed : C := ltac:(exact I.\n");
    let error = recovered
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "coq-parser-recovery-structure-unavailable")
        .expect("the unclosed Ltac term retains its native ERROR node");
    assert_eq!(error.source.span().start_byte(), 22);
    assert_eq!(error.source.span().end_byte(), 38);
    assert!(
        !recovered
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "unclosed")
    );
    for source in ["Instance _ : C := value.\n", "Declare Instance Type : C.\n"] {
        let result = output(source);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| { matches!(entity.canonical_name.as_str(), "_" | "Type") })
        );
        assert!(result.document().skipped_regions.iter().any(|gap| matches!(
            gap.detail.as_str(),
            "coq-declaration-name-unavailable" | "coq-parser-recovery-structure-unavailable"
        )));
    }
}

#[test]
fn coq_instance_identity_separates_declarations_and_ignores_body_edits() {
    let before = output("Instance provided : C := value.\n");
    let after = output("Instance provided : C := replacement.\n");
    let find = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "provided")
            .unwrap()
            .id
    };
    assert_eq!(find(&before), find(&after));
    for (result, expected) in [
        (&before, "Instance provided : C := value"),
        (&after, "Instance provided : C := replacement"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == find(result))
            .unwrap();
        let source = entity.evidence.source.as_ref().unwrap();
        assert_eq!(source.span().start_byte(), 0);
        assert_eq!(source.span().end_byte(), expected.len() as u64);
    }
    let repeated = output(
        "Instance same : C := value.\nModule Type Contract.\nDeclare Instance same : C.\nEnd Contract.\n",
    );
    let entities: Vec<_> = repeated
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "same")
        .collect();
    assert_eq!(entities.len(), 2);
    assert_ne!(entities[0].id, entities[1].id);
    for role in [OccurrenceRole::Definition, OccurrenceRole::Declaration] {
        assert_eq!(
            repeated
                .document()
                .occurrences
                .iter()
                .filter(|occurrence| occurrence.role == role)
                .count(),
            1 + usize::from(role == OccurrenceRole::Definition),
        );
    }
}

#[test]
fn coq_original_attributed_instances_remain_explicit_applicable_gaps() {
    for (source, name) in [
        (
            include_str!("../fixtures/coq-instances/dirpath_eqdec.v"),
            "dirpath_eqdec",
        ),
        (
            include_str!("../fixtures/coq-instances/reflect_prim_int.v"),
            "reflect_prim_int",
        ),
        (
            include_str!("../fixtures/coq-instances/ascii_eqdec.v"),
            "ascii_eqdec",
        ),
    ] {
        let result = output(source);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
        let gap = result
            .document()
            .skipped_regions
            .iter()
            .find(|gap| {
                gap.detail == "coq-fail-attribute-or-selector-declaration-effect-unavailable"
            })
            .unwrap();
        assert_eq!(gap.source.span().start_byte(), 0);
        assert_eq!(gap.source.span().end_byte(), source.len() as u64);
    }
}

#[test]
fn coq_projection_body_references_keep_exact_sources_and_unresolved_targets() {
    let text = "(* λ😀 *)\nDefinition argument := 0.\nDefinition result := value.(Module.field (argument := @supplied)).\nDefinition explicit := value.(@Module.field parameter).\nDefinition kept := 0.\n";
    for (encoding, bom) in [
        ("utf-8", false),
        ("utf-8", true),
        ("utf-16le", true),
        ("utf-16be", true),
    ] {
        let source = format!("{}{text}", if bom { "\u{feff}" } else { "" });
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
        let raw = encode(&source);
        let provider = Arc::new(provider());
        let fixture = Fixture::new(COQ, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(COQ),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, COQ),
            &request,
            &ExtensionSupport::default(),
        );
        validate(&result, &raw, encoding);
        named_definition(result.document(), "kept", EntityKind::Constant);
        let references: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .collect();
        let mut expected = Vec::new();
        for name in ["value", "Module.field", "supplied", "parameter"] {
            for (at, _) in source.match_indices(name) {
                let start = encode(&source[..at]).len();
                expected.push((
                    u64::try_from(start).unwrap(),
                    u64::try_from(start + encode(name).len()).unwrap(),
                    name,
                ));
            }
        }
        assert_eq!(
            references.len(),
            expected.len(),
            "{encoding}/{bom}: {references:?}"
        );
        for (start, end, name) in expected {
            let sites: Vec<_> = references
                .iter()
                .filter(|site| site.source.span().start_byte() == start)
                .collect();
            assert_eq!(sites.len(), 1, "{encoding}/{bom}: {name}");
            let site = sites[0];
            assert_eq!(site.source.span().end_byte(), end);
            assert_eq!(site.source.content_hash(), content_hash(&raw));
            assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
            assert_eq!(site.syntax_kind, "coq.written_reference.reference");
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert_eq!(
                result
                    .document()
                    .provenance
                    .iter()
                    .find(|p| p.id == site.provenance)
                    .unwrap()
                    .tier,
                AnalysisTier::TierD,
            );
        }
        assert!(result.document().skipped_regions.iter().any(|gap| gap.detail
            == "coq-local-term-binder-projection-and-generated-declaration-coverage-incomplete"));
    }
}

#[test]
fn coq_tactic_notation_is_not_a_global_declaration_or_resolved_effect() {
    let text = "(* λ😀 *)\nTactic Notation (at level 3) \"fabricated_function\" ident(H) := match type of H with | True => idtac | _ => idtac end.\nDefinition kept := 0.\n";
    for (encoding, bom) in [
        ("utf-8", false),
        ("utf-8", true),
        ("utf-16le", true),
        ("utf-16be", true),
    ] {
        let source = format!("{}{text}", if bom { "\u{feff}" } else { "" });
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
        let raw = encode(&source);
        let provider = Arc::new(provider());
        let fixture = Fixture::new(COQ, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(COQ),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, COQ),
            &request,
            &ExtensionSupport::default(),
        );
        validate(&result, &raw, encoding);
        let kept = named_definition(result.document(), "kept", EntityKind::Constant);
        let start = source.find("kept").unwrap();
        let end = start + "kept".len();
        let span = kept.source.span();
        assert_eq!(
            span.start_byte(),
            u64::try_from(encode(&source[..start]).len()).unwrap()
        );
        assert_eq!(
            span.end_byte(),
            u64::try_from(encode(&source[..end]).len()).unwrap()
        );
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.canonical_name != "fabricated_function"
                    && entity.canonical_name != "H")
        );
        assert!(
            result
                .document()
                .relations
                .iter()
                .all(|relation| relation.predicate != rootlight_ir::RelationPredicate::Calls)
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Reference)
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "coq-vernacular-construct-structure-unavailable")
        );
    }
}

#[test]
fn coq_written_namespace_competitors_keep_notation_references_unresolved() {
    for middle in [
        "Record holder := target { field : nat }.",
        "Record holder := make { target : nat }.",
        "Class holder := target { field : nat }.",
        "Class holder := make { target : nat }.",
        "Record holder := { field : nat }.",
        "Class holder := target : nat.",
        "Record holder `(x : C) := make { field : nat }.",
        "Record holder := make { field `(x : C) : nat }.",
        "Definition holder `(x : C) := x.",
    ] {
        let source = format!("Definition target := 0. {middle} Notation \"! T\" := (target T).\n");
        let result = certificate_output(&source);
        let document = result.document();
        named_definition(document, "target", EntityKind::Constant);
        let start = u64::try_from(source.rfind("target").unwrap()).unwrap();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == start && site.role == OccurrenceRole::Reference
            })
            .collect();
        assert_eq!(sites.len(), 1, "{source}");
        assert_eq!(sites[0].source.span().end_byte(), start + 6);
        assert!(
            matches!(sites[0].target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.source.span().start_byte() == start
                    && site.role == OccurrenceRole::Definition)
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo)
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "coq-declaration-time-notation-binding-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn coq_malformed_namespace_prefix_cannot_publish_a_notation_certificate() {
    let source = "Definition target := 0. Record holder := make { field : }. Notation \"! T\" := (target T).\n";
    let result = certificate_output(source);
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo)
    );
    assert!(result.document().skipped_regions.iter().any(|gap| matches!(
        gap.detail.as_str(),
        "coq-declaration-time-notation-binding-unavailable"
            | "coq-parser-recovery-structure-unavailable"
            | "syntax-error-recovery"
    )));
    let fixture = Fixture::new(COQ, source.as_bytes());
    let provider = Arc::new(provider());
    let limits = limits();
    let parsed = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &request(&fixture.snapshot, &fixture.source, COQ, &limits).to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert!(
        !parsed
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "coq.notation_target.signature")
    );
}

// Complete original callable requirements accompany a separate native observer.
// The observer reports actual geometry without requiring a future producer label.

const WRITTEN_CALLABLE_ORIGINAL: &[u8] =
    include_bytes!("../fixtures/coq-written-callables/stlc_small.v");

fn written_callable_original_entity<'document>(
    document: &'document rootlight_ir::NormalizedIrDocument,
    name: &str,
    start: u64,
    end: u64,
) -> &'document rootlight_ir::EntityRecord {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(definitions.len(), 1, "one exact original definition {name}");
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("a written definition must identify its own entity");
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.language, "coq");
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.provenance, definitions[0].provenance);
    assert_eq!(
        definitions[0].source.content_hash(),
        content_hash(WRITTEN_CALLABLE_ORIGINAL)
    );
    let evidence = entity.evidence.source.as_ref().unwrap();
    assert_eq!(
        evidence.content_hash(),
        content_hash(WRITTEN_CALLABLE_ORIGINAL)
    );
    assert_eq!(evidence.repository(), definitions[0].repository);
    assert_eq!(evidence.generation(), definitions[0].generation);
    assert_eq!(evidence.span().file(), definitions[0].file);
    assert!(evidence.span().start_byte() <= start && evidence.span().end_byte() >= end);
    entity
}

fn written_callable_original() -> AnalysisOutput {
    assert_eq!(WRITTEN_CALLABLE_ORIGINAL.len(), 2182);
    for (start, end, text) in [
        (11, 13, b"Ty".as_slice()),
        (16, 19, b"Set"),
        (1226, 1229, b"var"),
        (1622, 1624, b"v0"),
        (1656, 1659, b"var"),
    ] {
        assert_eq!(&WRITTEN_CALLABLE_ORIGINAL[start..end], text);
    }
    certificate_output(std::str::from_utf8(WRITTEN_CALLABLE_ORIGINAL).unwrap())
}

#[test]
fn original_callables_retain_exact_function_definitions_and_ordinary_constant() {
    let output = written_callable_original();
    let document = output.document();
    for (name, start, end) in [("var", 1226, 1229), ("v0", 1622, 1624)] {
        let entity = written_callable_original_entity(document, name, start, end);
        assert_eq!(entity.kind, EntityKind::Function, "written callable {name}");
    }
    let type_value = written_callable_original_entity(document, "Ty", 11, 13);
    assert_eq!(type_value.kind, EntityKind::Constant);
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 16
            && site.source.span().end_byte() == 19
    }));
    assert!(!document.skipped_regions.is_empty());
}

#[test]
fn original_v0_reference_selects_the_earlier_written_var_callable() {
    let output = written_callable_original();
    let document = output.document();
    let target = written_callable_original_entity(document, "var", 1226, 1229);
    let owner = written_callable_original_entity(document, "v0", 1622, 1624);
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 1656
                && site.source.span().end_byte() == 1659
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact original var reference");
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert_eq!(sites[0].enclosing, Some(owner.id));
    assert_eq!(sites[0].provenance, owner.provenance);
    assert_eq!(sites[0].provenance, target.provenance);
    assert_eq!(
        sites[0].source.content_hash(),
        content_hash(WRITTEN_CALLABLE_ORIGINAL)
    );
    assert_eq!(sites[0].repository, target.repository);
    assert_eq!(sites[0].generation, target.generation);
    assert_eq!(target.kind, EntityKind::Function);
    assert_eq!(owner.kind, EntityKind::Function);
}

fn written_callable_native_description(node: tree_sitter::Node<'_>) -> String {
    let field = node.parent().and_then(|parent| {
        (0..parent.child_count()).find_map(|index| {
            let index = u32::try_from(index).unwrap();
            (parent.child(index) == Some(node))
                .then(|| parent.field_name_for_child(index))
                .flatten()
        })
    });
    format!(
        "id={} parent={:?} kind={} field={field:?} bytes={}..{} error={} missing={} has_error={}",
        node.id(),
        node.parent().map(|parent| parent.id()),
        node.kind(),
        node.start_byte(),
        node.end_byte(),
        node.is_error(),
        node.is_missing(),
        node.has_error()
    )
}

#[test]
fn original_callables_native_raw_and_normalized_observation() {
    use rootlight_adapter_sdk::execute_parse;

    assert_eq!(WRITTEN_CALLABLE_ORIGINAL.len(), 2182);
    println!("source-sha256=a8b32fce2d84355b050fded6d794f313ca068080816a46620d1df69f2af16ffd");
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rocq::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(WRITTEN_CALLABLE_ORIGINAL, None).unwrap();
    let root = tree.root_node();
    assert!(
        root.start_byte() <= root.end_byte() && root.end_byte() <= WRITTEN_CALLABLE_ORIGINAL.len()
    );
    println!("native-root {}", written_callable_native_description(root));
    println!("native-full-sexp {}", root.to_sexp());
    let mut cursor = root.walk();
    let mut nodes = 0;
    let mut errors = 0;
    let mut missing = 0;
    loop {
        let node = cursor.node();
        nodes += 1;
        assert!(nodes <= 8192, "finite complete original observation");
        assert!(
            node.start_byte() <= node.end_byte()
                && node.end_byte() <= WRITTEN_CALLABLE_ORIGINAL.len()
        );
        errors += usize::from(node.is_error());
        missing += usize::from(node.is_missing());
        println!("native-node {}", written_callable_native_description(node));
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break;
            }
        }
        if cursor.node() == root {
            break;
        }
    }
    println!("native-summary nodes={nodes} errors={errors} missing={missing}");
    for (start, end, text) in [
        (1226, 1229, b"var".as_slice()),
        (1622, 1624, b"v0"),
        (1656, 1659, b"var"),
    ] {
        assert_eq!(&WRITTEN_CALLABLE_ORIGINAL[start..end], text);
        let mut current = Some(root.descendant_for_byte_range(start, end).unwrap());
        for _ in 0..256 {
            let Some(node) = current else { break };
            println!(
                "native-anchor={start}..{end} {}",
                written_callable_native_description(node)
            );
            if node.kind().ends_with("_command") {
                println!("native-owner={} sexp={}", node.kind(), node.to_sexp());
            }
            current = node.parent();
        }
        assert!(current.is_none(), "finite original anchor ancestry");
    }
    let native = Arc::new(provider());
    let fixture = Fixture::new(COQ, WRITTEN_CALLABLE_ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, COQ, &bounds);
    let parsed = execute_parse(
        native.as_ref(),
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    for fact in parsed.facts() {
        println!("raw-fact {fact:?}");
    }
    let result = analyze(
        &analyzer(&native, COQ),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default()).unwrap();
    for entity in &result.document().entities {
        println!("normalized-entity {entity:?}");
    }
    for occurrence in &result.document().occurrences {
        if [(1226, 1229), (1622, 1624), (1656, 1659)].contains(&(
            occurrence.source.span().start_byte(),
            occurrence.source.span().end_byte(),
        )) {
            println!("normalized-occurrence {occurrence:?}");
        }
    }
    for relation in &result.document().relations {
        println!("normalized-relation {relation:?}");
    }
    for gap in &result.document().skipped_regions {
        println!("normalized-gap {gap:?}");
    }
}

// Written callable classification and lexical targets do not assert elaboration.
fn written_callable_reference(
    document: &rootlight_ir::NormalizedIrDocument,
    start: u64,
    end: u64,
) -> &rootlight_ir::OccurrenceRecord {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1);
    sites[0]
}

#[test]
fn written_formals_and_direct_lambda_classify_callables_without_promoting_values_or_quantifiers() {
    let source = "Definition scalar := 0.\nDefinition quantified := forall (A : Set), A -> A.\nDefinition lambda := fun x => x.\nDefinition choose (n : nat) : nat := match n with | O => O | S m => m end.\nDefinition renamed {A} (x : A) : A := x.\n";
    let output = certificate_output(source);
    for name in ["lambda", "choose", "renamed"] {
        named_definition(output.document(), name, EntityKind::Function);
    }
    for name in ["scalar", "quantified"] {
        named_definition(output.document(), name, EntityKind::Constant);
    }
    assert!(
        output
            .document()
            .entities
            .iter()
            .all(|entity| !matches!(entity.canonical_name.as_str(), "A" | "n" | "x" | "m"))
    );
    assert!(output.document().skipped_regions.iter().any(|gap| gap.detail == "coq-term-and-module-binder-entities-and-lifetime-unavailable"));
    assert!(
        output
            .document()
            .relations
            .iter()
            .all(|relation| relation.predicate != RelationPredicate::Calls)
    );
}

#[test]
fn direct_application_targets_require_an_earlier_unique_callable_and_unshadowed_closed_environment()
{
    let source =
        "Definition renamed (x : nat) : nat := x. Definition owned (y : nat) : nat := renamed y.\n";
    let output = certificate_output(source);
    let definition = named_definition(output.document(), "renamed", EntityKind::Function);
    let OccurrenceTarget::Resolved { symbol } = definition.target else {
        panic!("written target")
    };
    let start = u64::try_from(source.rfind("renamed").unwrap()).unwrap();
    assert_eq!(
        written_callable_reference(output.document(), start, start + 7).target,
        OccurrenceTarget::Resolved { symbol }
    );
    for source in [
        "Require Import Other. Definition target (x : nat) := x. Definition owned (y : nat) := target y.",
        "Section S. Definition target (x : nat) := x. Definition owned (y : nat) := target y. End S.",
        "Definition target (x : nat) := x. Definition owned (target : nat) := target 0.",
        "Definition target (x : nat) := x. Definition target (x : nat) := x. Definition owned (y : nat) := target y.",
        "Definition owned (y : nat) := target y. Definition target (x : nat) := x.",
        "Definition target := 0. Definition owned (y : nat) := target y.",
        "Fixpoint target (x : nat) := x. Definition owned (y : nat) := target y.",
        "Definition target (x : nat) := x. Definition owned (y : nat) := let target := y in target 0.",
        "Definition target (x : nat) := x. Definition owned (y : nat) := (fun target => target y) target.",
    ] {
        let output = certificate_output(source);
        assert!(
            output
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Reference)
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
            "{source}"
        );
        assert!(
            output.document().relations.iter().all(|edge| !matches!(
                edge.predicate,
                RelationPredicate::Calls | RelationPredicate::RefersTo
            )),
            "{source}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail
                    == "coq-name-resolution-and-implicit-argument-elaboration-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn callable_original_required_artifact_exact_and_one_less_preserve_current_generation_and_source() {
    let fixture = Fixture::new(COQ, WRITTEN_CALLABLE_ORIGINAL);
    let native = Arc::new(provider());
    let frontend = analyzer(&native, COQ);
    let wide = limits();
    let input = request(&fixture.snapshot, &fixture.source, COQ, &wide);
    let required = native
        .required_syntax_fact_count(&input.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 4);
    let exact = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, COQ, &exact);
    let (output, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    assert!(matches!(
        written_callable_reference(output.document(), 1656, 1659).target,
        OccurrenceTarget::Resolved { .. }
    ));
    let next = fixture.next_generation();
    let input = request(&next.snapshot, &next.source, COQ, &exact);
    let reused = frontend
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&frontend, &input, &ExtensionSupport::default());
    assert_eq!(reused.document(), fresh.document());
    assert!(
        reused
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.generation() == next.source.generation())
    );
    let below = limits_with_syntax_records(required - 1);
    let input = request(&fixture.snapshot, &fixture.source, COQ, &below);
    for error in [
        frontend.prepare_artifact(&input, &deadline()).unwrap_err(),
        rootlight_adapter_sdk::execute_parse(
            native.as_ref(),
            &input.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err(),
    ] {
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }) if observed == required && limit == required - 1),
            "{error:?}"
        );
    }
    let changed = fixture.rewrite(&[WRITTEN_CALLABLE_ORIGINAL, b" "].concat());
    let mismatch = frontend
        .analyze_from_artifact(
            &request(&changed.snapshot, &changed.source, COQ, &wide),
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(mismatch, AdapterError::ProviderFailed { code } if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
}

fn validate_written_callable_original(
    result: &AnalysisOutput,
    source: &[u8],
    encoding: &str,
    coordinate: impl Fn(usize) -> u64,
) {
    let document = result.document();
    validate_ir_document(document, limits().ir(), &ExtensionSupport::default()).unwrap();
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    let mut certified_references = Vec::new();
    for (owner_name, owner_start, owner_end, target_name, target_start, target_end, start, end) in [
        ("v0", 1622, 1624, "var", 1226, 1229, 1656, 1659),
        ("v1", 1678, 1680, "var", 1226, 1229, 1723, 1726),
        ("v2", 1750, 1752, "var", 1226, 1229, 1806, 1809),
        ("v3", 1838, 1840, "var", 1226, 1229, 1905, 1908),
        ("v4", 1942, 1944, "var", 1226, 1229, 2020, 2023),
        ("test", 2062, 2066, "lam", 1319, 1322, 2113, 2116),
    ] {
        assert_eq!(
            &WRITTEN_CALLABLE_ORIGINAL[owner_start..owner_end],
            owner_name.as_bytes()
        );
        assert_eq!(
            &WRITTEN_CALLABLE_ORIGINAL[target_start..target_end],
            target_name.as_bytes()
        );
        assert_eq!(
            &WRITTEN_CALLABLE_ORIGINAL[start..end],
            target_name.as_bytes()
        );
        let owner = named_definition(document, owner_name, EntityKind::Function);
        let target = named_definition(document, target_name, EntityKind::Function);
        assert_eq!(
            (
                owner.source.span().start_byte(),
                owner.source.span().end_byte()
            ),
            (coordinate(owner_start), coordinate(owner_end))
        );
        assert_eq!(
            (
                target.source.span().start_byte(),
                target.source.span().end_byte()
            ),
            (coordinate(target_start), coordinate(target_end))
        );
        let OccurrenceTarget::Resolved { symbol: owner_id } = owner.target else {
            panic!("written callable owner");
        };
        let OccurrenceTarget::Resolved { symbol: target_id } = target.target else {
            panic!("written callable target");
        };
        let site = written_callable_reference(document, coordinate(start), coordinate(end));
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target_id }
        );
        assert_eq!(site.enclosing, Some(owner_id));
        assert_eq!(site.provenance, owner.provenance);
        assert_eq!(site.provenance, target.provenance);
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
            })
            .collect();
        assert_eq!(edges.len(), 1);
        let edge = edges[0];
        assert_eq!(edge.object, RelationEndpoint::Entity(target_id));
        assert_eq!(edge.repository, document.repository);
        assert_eq!(edge.generation, document.generation);
        assert_eq!(edge.provenance, site.provenance);
        assert_eq!(edge.confidence, site.confidence);
        assert_eq!(edge.evidence_kind, rootlight_ir::EvidenceKind::Derived);
        assert_eq!(edge.evidence.source.as_ref(), Some(&site.source));
        certified_references.push(site.id);
    }
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo)
            .count(),
        certified_references.len()
    );
    assert!(document.relations.iter().all(|edge| {
        edge.predicate == RelationPredicate::Contains
            || (edge.predicate == RelationPredicate::RefersTo
                && matches!(edge.subject, RelationEndpoint::Occurrence(id)
                    if certified_references.contains(&id)))
    }));
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "coq-elaboration-typechecking-proofchecking-and-term-binding-unavailable"));
    for occurrence in &document.occurrences {
        let span = occurrence.source.span();
        let bytes = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        let spelling = if encoding == "utf-8" {
            std::str::from_utf8(bytes).unwrap().to_owned()
        } else {
            assert_eq!(bytes.len() % 2, 0);
            String::from_utf16(
                &bytes
                    .chunks_exact(2)
                    .map(|unit| {
                        let unit = [unit[0], unit[1]];
                        if encoding == "utf-16le" {
                            u16::from_le_bytes(unit)
                        } else {
                            u16::from_be_bytes(unit)
                        }
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(spelling.as_bytes()),
            "{encoding}: {}",
            occurrence.syntax_kind
        );
        assert_eq!(occurrence.source.content_hash(), content_hash(source));
        assert_eq!(occurrence.source.generation(), document.generation);
        assert_eq!(occurrence.source.repository(), document.repository);
        assert_ne!(occurrence.role, OccurrenceRole::CallSite);
        if occurrence.role == OccurrenceRole::Declaration {
            assert!(matches!(
                occurrence.syntax_kind.as_str(),
                "coq.declared_instance_name.definition"
                    | "coq.record_field_name.definition"
                    | "coq.class_field_name.definition"
            ));
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { .. }
            ));
        } else if occurrence.role != OccurrenceRole::Definition {
            if certified_references.contains(&occurrence.id) {
                assert_eq!(occurrence.role, OccurrenceRole::Reference);
                assert!(matches!(
                    occurrence.target,
                    OccurrenceTarget::Resolved { .. }
                ));
            } else {
                assert!(matches!(
                    occurrence.target,
                    OccurrenceTarget::Unresolved { .. }
                ));
            }
        }
    }
}

#[test]
fn written_callable_original_bom_and_utf16_retain_physical_reference_and_definition_coordinates() {
    let plain = std::str::from_utf8(WRITTEN_CALLABLE_ORIGINAL).unwrap();
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| -> Vec<u8> {
            let text = format!("\u{feff}{text}");
            if encoding == "utf-8" {
                return text.into_bytes();
            }
            text.encode_utf16()
                .flat_map(|unit| {
                    if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect()
        };
        let bytes = encode(plain);
        let fixture = Fixture::new(COQ, &bytes);
        let native = Arc::new(provider());
        let budget = limits();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(COQ),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let output = analyze(
            &analyzer(&native, COQ),
            &input,
            &ExtensionSupport::default(),
        );
        validate_written_callable_original(&output, &bytes, encoding, |at| {
            u64::try_from(encode(&plain[..at]).len()).unwrap()
        });
        let target = named_definition(output.document(), "var", EntityKind::Function);
        let expected_definition = (
            u64::try_from(encode(&plain[..1226]).len()).unwrap(),
            u64::try_from(encode(&plain[..1229]).len()).unwrap(),
        );
        assert_eq!(
            (
                target.source.span().start_byte(),
                target.source.span().end_byte()
            ),
            expected_definition
        );
        let start = u64::try_from(encode(&plain[..1656]).len()).unwrap();
        let end = u64::try_from(encode(&plain[..1659]).len()).unwrap();
        let site = written_callable_reference(output.document(), start, end);
        assert_eq!(site.target, target.target);
        assert_eq!(site.source.content_hash(), content_hash(&bytes));
        assert_eq!(site.source.repository(), fixture.source.repository());
        assert_eq!(site.source.generation(), fixture.source.generation());
    }
}

#[test]
fn callable_binding_requires_singular_raw_root_context_header_declaration_definition_and_marker() {
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
    let native = provider();
    let fixture = Fixture::new(COQ, WRITTEN_CALLABLE_ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, COQ, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts| {
        let parser = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(parser),
            ProducerIdentity::new(
                "coq-written-replay",
                "1",
                content_hash(b"coq-written-replay"),
            )
            .unwrap(),
            language(COQ),
            COQ.frontend,
            content_hash(b"coq-written-replay-binary"),
        )
        .unwrap();
        execute_analysis(
            &frontend,
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap()
    };
    let baseline = replay(parsed.facts().to_vec());
    assert!(matches!(
        written_callable_reference(baseline.document(), 1656, 1659).target,
        OccurrenceTarget::Resolved { .. }
    ));
    let modules: Vec<_> = baseline
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Module)
        .collect();
    assert_eq!(modules.len(), 1);
    for name in ["var", "v0"] {
        let definition = named_definition(baseline.document(), name, EntityKind::Function);
        let OccurrenceTarget::Resolved { symbol } = definition.target else {
            panic!("written callable definition");
        };
        let entity = baseline
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.container, Some(ContainerRef::Entity(modules[0].id)));
    }
    for (label, start) in [
        ("coq.proof_checking.signature", 0),
        ("coq.written_target.signature", 1215),
        ("coq.written_header.signature", 1215),
        ("coq.written_header.signature", 1611),
        ("coq.written_callable.declaration", 1215),
        ("coq.name.definition", 1226),
    ] {
        for duplicate in [false, true] {
            let mut facts = parsed.facts().to_vec();
            let selected: Vec<_> = facts
                .iter()
                .filter(|fact| {
                    fact.syntax_kind().as_str() == label
                        && fact.span().start_byte() == start
                        && (label != "coq.written_target.signature"
                            || fact.span().end_byte() == 1659)
                })
                .cloned()
                .collect();
            assert_eq!(selected.len(), 1, "baseline witness {label}:{start}");
            let witness = &selected[0];
            if duplicate {
                facts.push(SyntaxFact::new(
                    facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
                    witness.parent(),
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    witness.syntax_kind().clone(),
                ));
            } else {
                facts.retain(|fact| fact.local_id() != witness.local_id());
                if witness.kind() == SyntaxFactKind::Declaration {
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
                }
            }
            let output = replay(facts);
            assert!(
                matches!(
                    written_callable_reference(output.document(), 1656, 1659).target,
                    OccurrenceTarget::Unresolved { .. }
                ),
                "{label} duplicate={duplicate}"
            );
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail
                        == "coq-name-resolution-and-implicit-argument-elaboration-unavailable")
            );
        }
    }
    let root = parsed
        .facts()
        .iter()
        .find(|fact| fact.syntax_kind().as_str() == "coq.source_file.root")
        .unwrap();
    let module = parsed
        .facts()
        .iter()
        .find(|fact| fact.syntax_kind().as_str() == "coq.file.module")
        .unwrap();
    let context = parsed
        .facts()
        .iter()
        .find(|fact| fact.syntax_kind().as_str() == "coq.proof_checking.signature")
        .unwrap();
    for mutant in 0..4 {
        let mut facts = parsed.facts().to_vec();
        match mutant {
            0 | 1 => facts.push(SyntaxFact::new(
                facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
                module.parent(),
                module.kind(),
                module.span(),
                module.depth(),
                if mutant == 0 {
                    module.syntax_kind().clone()
                } else {
                    rootlight_adapter_sdk::SyntaxKindLabel::new("foreign.file.module").unwrap()
                },
            )),
            2 | 3 => {
                let witness = if mutant == 2 { module } else { context };
                let selected = facts
                    .iter_mut()
                    .find(|fact| fact.local_id() == witness.local_id())
                    .unwrap();
                *selected = SyntaxFact::new(
                    witness.local_id(),
                    if mutant == 2 {
                        witness.parent()
                    } else {
                        Some(root.local_id())
                    },
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    if mutant == 2 {
                        rootlight_adapter_sdk::SyntaxKindLabel::new("foreign.file.module").unwrap()
                    } else {
                        witness.syntax_kind().clone()
                    },
                );
            }
            _ => unreachable!(),
        }
        let output = replay(facts);
        assert!(
            matches!(
                written_callable_reference(output.document(), 1656, 1659).target,
                OccurrenceTarget::Unresolved { .. }
            ),
            "module/context mutant {mutant}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail
                    == "coq-name-resolution-and-implicit-argument-elaboration-unavailable")
        );
    }
}
