//! Written Sed labels and branch dependencies retain exact physical source.
//! The static file-text view does not qualify raw-program runtime semantics.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/sed/mario.sed");
const SED: LanguageCase = LanguageCase {
    name: "sed",
    path: "written.sed",
    frontend: "sed-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(SED, bytes);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(SED),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, SED),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    for site in &result.document().occurrences {
        assert_eq!(site.source.repository(), fixture.source.repository());
        assert_eq!(site.source.generation(), fixture.source.generation());
        assert_eq!(site.source.span().file(), fixture.source.span().file());
        assert_eq!(site.source.content_hash(), content_hash(bytes));
        assert!(site.source.span().end_byte() <= u64::try_from(bytes.len()).unwrap());
    }
    result
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    start: u64,
    end: u64,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "exact Sed definition {name}@{start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("written definition must own its entity");
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::StatementLabel);
    assert_eq!(entity.language, "sed");
    assert_eq!(entity.canonical_name, name);
    symbol
}

fn original_contract(document: &rootlight_ir::NormalizedIrDocument, position: impl Fn(u64) -> u64) {
    let none = definition(document, "none", position(975), position(979));
    let left = definition(document, "left", position(990), position(994));
    let right = definition(document, "right", position(1020), position(1025));
    let end = definition(document, "end", position(5403), position(5406));
    let module = document
        .entities
        .iter()
        .find(|entity| entity.language == "sed" && entity.kind == EntityKind::Module)
        .unwrap();
    for symbol in [none, left, right, end] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::Entity(module.id))
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.subject == RelationEndpoint::Entity(symbol)
                    && edge.predicate == RelationPredicate::Contains)
        );
    }
    for (owner, start, stop) in [(none, 984, 987), (left, 1014, 1017)] {
        let start = position(start);
        let stop = position(stop);
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == stop
            })
            .collect();
        assert_eq!(sites.len(), 1);
        let site = sites[0];
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: end });
        assert_eq!(site.enclosing, Some(module.id));
        for subject in [
            RelationEndpoint::Occurrence(site.id),
            RelationEndpoint::Entity(owner),
        ] {
            let edges: Vec<_> = document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::RefersTo
                        && edge.subject == subject
                        && edge.object == RelationEndpoint::Entity(end)
                        && edge
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source == &site.source)
                })
                .collect();
            assert_eq!(edges.len(), 1, "one exact native written dependency");
            assert_eq!(edges[0].provenance, site.provenance);
            if subject == RelationEndpoint::Entity(owner) {
                assert_eq!(edges[0].evidence.derivation.len(), 3);
                for proof in [
                    rootlight_ir::FactRef::Entity(owner),
                    rootlight_ir::FactRef::Fact(site.id),
                    rootlight_ir::FactRef::Entity(end),
                ] {
                    assert!(edges[0].evidence.derivation.contains(&proof));
                }
            }
        }
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail
            .contains("raw-carriage-return-runtime-semantics-unqualified")
    }));
}

#[test]
fn full_original_sed_has_four_exact_labels_and_two_written_section_dependencies() {
    assert_eq!(ORIGINAL.len(), 8893);
    assert_eq!(
        content_hash(ORIGINAL).to_string(),
        "b3_untefh2tstnrgqj74ikoxuxayelmoallhirnayo3r7apw6nowpdr7ohqvm"
    );
    let result = output(ORIGINAL, EncodingId::new("utf-8").unwrap());
    original_contract(result.document(), |offset| offset);
}

#[test]
fn encoded_original_sed_keeps_physical_label_and_branch_provenance() {
    for encoding in ["utf-8-bom", "utf-16le", "utf-16be"] {
        let mut bytes = Vec::new();
        if encoding == "utf-8-bom" {
            bytes.extend_from_slice(b"\xef\xbb\xbf");
            bytes.extend_from_slice(ORIGINAL);
        } else {
            bytes.extend_from_slice(if encoding == "utf-16le" {
                b"\xff\xfe"
            } else {
                b"\xfe\xff"
            });
            for unit in std::str::from_utf8(ORIGINAL).unwrap().encode_utf16() {
                bytes.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        let result = output(
            &bytes,
            EncodingId::new(if encoding == "utf-8-bom" {
                "utf-8"
            } else {
                encoding
            })
            .unwrap(),
        );
        original_contract(result.document(), |offset| {
            if encoding == "utf-8-bom" {
                offset + 3
            } else {
                offset * 2 + 2
            }
        });
    }
}

#[test]
fn duplicate_missing_and_opaque_label_targets_remain_unresolved() {
    for source in [
        ":owner\nb target\n:target\n:target\n",
        ":owner\nb absent\n:target\n",
        ":owner\nb target\n:opaque name\n:target\n",
    ] {
        let result = output(source.as_bytes(), EncodingId::new("utf-8").unwrap());
        let document = result.document();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .collect();
        assert_eq!(sites.len(), 1);
        assert!(matches!(
            sites[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo)
        );
        assert!(document.skipped_regions.iter().any(|gap| gap.detail == "sed-written-branch-unique-local-label-target-unavailable"));
        if source.matches(":target").count() == 2 {
            let targets: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::StatementLabel && entity.canonical_name == "target"
                })
                .collect();
            assert_eq!(targets.len(), 2);
            assert_ne!(targets[0].id, targets[1].id);
        }
    }
}

#[test]
fn nested_addressed_and_control_crossing_branches_have_no_label_owner_dependency() {
    for source in [
        ":owner\n{\nb target\n}\n:target\n",
        ":owner\n1b target\n:target\n",
        ":owner\nq\nb target\n:target\n",
    ] {
        let result = output(source.as_bytes(), EncodingId::new("utf-8").unwrap());
        let document = result.document();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .collect();
        assert_eq!(sites.len(), 1);
        assert!(matches!(sites[0].target, OccurrenceTarget::Resolved { .. }));
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                    && matches!(edge.subject, RelationEndpoint::Occurrence(_)))
                .count(),
            1
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && matches!(edge.subject, RelationEndpoint::Entity(_)))
        );
        assert!(
            document.skipped_regions.iter().any(
                |gap| gap.detail == "sed-direct-sibling-written-section-dependency-unavailable"
            )
        );
    }
}

#[test]
fn comments_regex_text_and_operandless_branches_cannot_fabricate_labels_or_calls() {
    let source = "# :ghost b target\ns/:ghost b target/kept/\n:owner\nb\n:target\n";
    let result = output(source.as_bytes(), EncodingId::new("utf-8").unwrap());
    let document = result.document();
    let labels: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::StatementLabel)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(labels.len(), 2);
    assert!(labels.contains(&"owner") && labels.contains(&"target"));
    assert!(!document.occurrences.iter().any(|site| matches!(
        site.role,
        OccurrenceRole::Reference | OccurrenceRole::CallSite
    )));
    assert!(!document.relations.iter().any(|edge| matches!(
        edge.predicate,
        RelationPredicate::Calls | RelationPredicate::RefersTo
    )));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "sed-explicit-written-branch-label-operand-unavailable")
    );
}

#[test]
fn a_written_self_branch_uses_unique_derivation_parents_without_a_label_body() {
    let result = output(b":loop\nb loop\n", EncodingId::new("utf-8").unwrap());
    let document = result.document();
    let label = definition(document, "loop", 1, 5);
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Entity(label)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].object, RelationEndpoint::Entity(label));
    assert_eq!(edges[0].evidence.derivation.len(), 2);
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(label))
    );
}

#[test]
fn full_original_required_count_matches_artifact_and_refuses_one_less() {
    let provider = Arc::new(provider());
    let frontend = analyzer(&provider, SED);
    let fixture = Fixture::new(SED, ORIGINAL);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, SED, &budget);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 0);
    let exact_budget = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, SED, &exact_budget);
    let (output, artifact) = frontend
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    original_contract(output.document(), |at| at);
    let replay = frontend
        .analyze_from_artifact(
            &exact,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(replay.document(), output.document());
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, SED, &exact_budget);
    let rebound = frontend
        .analyze_from_artifact(
            &next_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    original_contract(rebound.document(), |at| at);
    assert_eq!(
        rebound.document(),
        analyze(&frontend, &next_request, &ExtensionSupport::default()).document()
    );
    let small_budget = limits_with_syntax_records(required - 1);
    let small = request(&fixture.snapshot, &fixture.source, SED, &small_budget);
    assert_eq!(
        provider
            .required_syntax_fact_count(&small.to_parse_request(), &deadline())
            .unwrap(),
        required
    );
    for error in [
        frontend.prepare_artifact(&small, &deadline()).unwrap_err(),
        rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
            &small.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err(),
    ] {
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts, observed, limit,
        }) if observed == required && limit == required - 1),
            "{error:?}"
        );
    }
}
