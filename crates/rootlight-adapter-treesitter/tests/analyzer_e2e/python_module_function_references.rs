//! Complete original guarded-module References keep separate native CallSite facts.

use super::*;
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
use rootlight_ir::ContainerRef;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/python_module_function/repl.py");
const LABEL: &str = "python.module_function.reference";

fn case() -> LanguageCase {
    LanguageCase {
        name: "python",
        path: "written.py",
        frontend: "written-source-regression",
        source: "",
        generated: false,
        body_before: "",
        body_after: "",
    }
}

fn output(bytes: &[u8]) -> AnalysisOutput {
    let native = Arc::new(provider());
    let case = case();
    let fixture = Fixture::new(case, bytes);
    let bounds = limits();
    let result = analyze(
        &analyzer(&native, case),
        &request(&fixture.snapshot, &fixture.source, case, &bounds),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files[0].content_hash, content_hash(bytes));
    result
}

fn certified(document: &rootlight_ir::NormalizedIrDocument, name: (u64, u64), at: (u64, u64)) {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == name.0
                && site.source.span().end_byte() == name.1
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("exact function Definition")
    };
    let function = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(function.kind, EntityKind::Function);
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == LABEL
                && site.source.span().start_byte() == at.0
                && site.source.span().end_byte() == at.1
        })
        .collect();
    assert_eq!(references.len(), 1);
    let site = references[0];
    assert_eq!(site.role, OccurrenceRole::Reference);
    assert!(
        matches!(site.target, OccurrenceTarget::Resolved { symbol: target } if target == symbol)
    );
    assert_eq!(site.enclosing.map(ContainerRef::Entity), function.container);
    assert_eq!(
        site.source.content_hash(),
        definitions[0].source.content_hash()
    );
    assert_eq!(site.provenance, function.provenance);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(symbol)
                && edge.evidence.source.as_ref() == Some(&site.source))
            .count(),
        1
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|other| other.role == OccurrenceRole::Definition
                && other.source.span() == site.source.span())
    );
    assert!(
        document
            .occurrences
            .iter()
            .any(|call| call.role == OccurrenceRole::CallSite
                && call.source.span().start_byte() <= at.0
                && call.source.span().end_byte() >= at.1),
        "the distinct native CallSite must survive"
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(site.id)),
        "Reference is not promoted to Calls"
    );
}

fn no_certificate(document: &rootlight_ir::NormalizedIrDocument) {
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == LABEL
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

fn refused_certificate(document: &rootlight_ir::NormalizedIrDocument) {
    no_certificate(document);
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == LABEL
                && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "python-native-module-function-binding-unavailable")
    );
}

#[test]
fn complete_original_resolves_exact_module_main_reference_and_keeps_native_call_sites() {
    assert_eq!(ORIGINAL.len(), 1818);
    assert_eq!(&ORIGINAL[1572..1576], b"main");
    assert_eq!(&ORIGINAL[1810..1814], b"main");
    let result = output(ORIGINAL);
    certified(result.document(), (1572, 1576), (1810, 1814));
}

#[test]
fn renamed_plain_entry_preserves_deferred_imports_without_qualifying_their_calls() {
    let source = b"import sys\n\ndef launch():\n    import worker\n    worker.run()\n\nif __name__ == '__main__':\n    launch()\n";
    let first = source
        .windows(6)
        .position(|word| word == b"launch")
        .unwrap() as u64;
    let last = source
        .windows(6)
        .rposition(|word| word == b"launch")
        .unwrap() as u64;
    let result = output(source);
    certified(result.document(), (first, first + 6), (last, last + 6));
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls),
        "this Reference proof provides no imported call authority"
    );
}

#[test]
fn competing_names_evaluation_star_dynamic_and_nonmodule_uses_remain_unproven() {
    let sources: &[&[u8]] = &[
        b"def run(): pass\nrun = other\nif __name__ == '__main__': run()\n",
        b"def run(): pass\ndef run(): pass\nif __name__ == '__main__': run()\n",
        b"from library import *\ndef run(): pass\nif __name__ == '__main__': run()\n",
        b"@replace\ndef run(): pass\nif __name__ == '__main__': run()\n",
        b"def run(arg=effect()): pass\nif __name__ == '__main__': run()\n",
        b"def run(): pass\nimport library\nif __name__ == '__main__': run()\n",
        b"def run():\n    globals()['run'] = other\nif __name__ == '__main__': run()\n",
        b"def run(): pass\ndef owner(run):\n    run()\n",
        b"def run(): pass\nif __name__ == '__main__':\n    run()\n    run()\n",
    ];
    for source in sources {
        no_certificate(output(source).document());
    }
}

#[test]
fn selected_full_extent_and_split_inputs_never_supply_complete_input_authority() {
    let case = case();
    let native = Arc::new(provider());
    let frontend = analyzer(&native, case);
    let fixture = Fixture::new(case, ORIGINAL);
    let bounds = limits();
    for selected in [
        vec![(0, 1818)],
        vec![(1775, 1818)],
        vec![(0, 1774), (1775, 1818)],
    ] {
        let ranges = selected
            .into_iter()
            .map(|(start, end)| {
                IncludedRange::new(
                    SourceSpan::new(fixture.source.span().file(), start, end).unwrap(),
                    language(case),
                )
            })
            .collect();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case),
            EncodingId::utf8(),
            ranges,
            AnalysisTier::TierD,
            build_context(),
            &bounds,
        )
        .unwrap()
        .with_generated_status(false);
        no_certificate(analyze(&frontend, &input, &ExtensionSupport::default()).document());
    }
}

#[test]
fn equal_spelling_definition_geometry_and_changed_current_bytes_refuse_native_fact_forgery() {
    let native = provider();
    let case = case();
    let fixture = Fixture::new(case, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts, selected: &Fixture| {
        let mock = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(mock),
            ProducerIdentity::new(
                "python-module-function-proof",
                "1",
                content_hash(b"python-module-function-proof"),
            )
            .unwrap(),
            language(case),
            case.frontend,
            content_hash(b"python-module-function-proof-binary"),
        )
        .unwrap();
        analyze(
            &frontend,
            &request(&selected.snapshot, &selected.source, case, &bounds),
            &ExtensionSupport::default(),
        )
    };
    certified(
        replay(parsed.facts().to_vec(), &fixture).document(),
        (1572, 1576),
        (1810, 1814),
    );
    let mut forged = parsed.facts().to_vec();
    let matching: Vec<_> = forged
        .iter()
        .enumerate()
        .filter(|(_, fact)| {
            fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "python.identifier.definition"
                && fact.span().start_byte() == 1572
                && fact.span().end_byte() == 1576
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(matching.len(), 1);
    let at = matching[0];
    let fact = &forged[at];
    forged[at] = SyntaxFact::new(
        fact.local_id(),
        fact.parent(),
        fact.kind(),
        SourceSpan::new(fact.span().file(), 1676, 1680).unwrap(),
        fact.depth(),
        fact.syntax_kind().clone(),
    );
    assert_eq!(
        &ORIGINAL[1676..1680],
        b"main",
        "same spelling inside the real function extent"
    );
    refused_certificate(replay(forged, &fixture).document());
    for label in [
        "python.module.root",
        "python.file.module",
        "python.function.declaration",
        "python.identifier.definition",
        LABEL,
    ] {
        let mut facts = parsed.facts().to_vec();
        let matching: Vec<_> = facts
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == label
                    && (label != "python.identifier.definition" || fact.span().start_byte() == 1572)
            })
            .cloned()
            .collect();
        assert_eq!(matching.len(), 1, "one original witness for {label}");
        let fact = &matching[0];
        let local = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
        facts.push(SyntaxFact::new(
            local,
            fact.parent(),
            fact.kind(),
            fact.span(),
            fact.depth(),
            fact.syntax_kind().clone(),
        ));
        refused_certificate(replay(facts, &fixture).document());
    }
    let mut bytes = ORIGINAL.to_vec();
    bytes[1810..1814].copy_from_slice(b"nope");
    let changed = fixture.rewrite(&bytes);
    no_certificate(replay(parsed.facts().to_vec(), &changed).document());
}

#[test]
fn reference_relations_and_records_use_the_real_exact_ir_admission_boundary() {
    let native = Arc::new(provider());
    let case = case();
    let frontend = analyzer(&native, case);
    let source = b"def launch(): pass\nif __name__ == '__main__': launch()\n";
    let fixture = Fixture::new(case, source);
    let base = limits();
    let run = |ir| {
        let bounds = AnalysisLimits::new(
            base.max_source_bytes(),
            base.max_syntax_nodes(),
            base.max_syntax_depth(),
            base.max_embedded_ranges(),
            base.max_reported_memory_bytes(),
            base.syntax_stream().clone(),
            base.ir_stream().clone(),
            ir,
        )
        .unwrap();
        frontend
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, case, &bounds),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    let baseline = run(IrLimits::default()).unwrap();
    let first = source
        .windows(6)
        .position(|word| word == b"launch")
        .unwrap() as u64;
    let last = source
        .windows(6)
        .rposition(|word| word == b"launch")
        .unwrap() as u64;
    certified(baseline.document(), (first, first + 6), (last, last + 6));
    for relations in [true, false] {
        let defaults = IrLimits::default();
        let mut low = 0;
        let mut high = if relations {
            defaults.max_relations
        } else {
            defaults.max_total_records
        };
        let set = |cap| {
            let mut ir = IrLimits::default();
            if relations {
                ir.max_relations = cap;
            } else {
                ir.max_total_records = cap;
            }
            ir
        };
        while low < high {
            let middle = low + (high - low) / 2;
            match run(set(middle)) {
                Ok(_) => high = middle,
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("unexpected admission failure: {error:?}"),
            }
        }
        assert!(low > 0);
        assert_eq!(run(set(low)).unwrap().document(), baseline.document());
        let error = run(set(low - 1)).unwrap_err();
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::Records, observed, limit }) if observed > limit && limit == low - 1),
            "{error:?}"
        );
    }
}

#[test]
fn exact_required_admission_and_real_artifact_replay_preserve_current_source_proof() {
    let native = Arc::new(provider());
    let case = case();
    let frontend = analyzer(&native, case);
    let fixture = Fixture::new(case, ORIGINAL);
    let wide = limits();
    let required = native
        .required_syntax_fact_count(
            &request(&fixture.snapshot, &fixture.source, case, &wide).to_parse_request(),
            &deadline(),
        )
        .unwrap();
    let bounds = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let (result, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    certified(result.document(), (1572, 1576), (1810, 1814));
    let rebound = frontend
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(result.document(), rebound.document());
    let next = fixture.next_generation();
    let next_input = request(&next.snapshot, &next.source, case, &bounds);
    let rebound = frontend
        .analyze_from_artifact(
            &next_input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        rebound.document(),
        analyze(&frontend, &next_input, &ExtensionSupport::default()).document()
    );
    let low = limits_with_syntax_records(required - 1);
    let small = request(&fixture.snapshot, &fixture.source, case, &low);
    assert!(matches!(frontend.prepare_artifact(&small, &deadline()),
        Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }))
        if observed == required && limit == required - 1));
    let mut changed = ORIGINAL.to_vec();
    changed[1810] = b'x';
    let changed = fixture.rewrite(&changed);
    assert!(
        frontend
            .analyze_from_artifact(
                &request(&changed.snapshot, &changed.source, case, &bounds),
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
}

#[test]
fn required_native_count_matches_full_and_exact_artifacts_with_distinct_callsite() {
    let native = Arc::new(provider());
    let case = case();
    let frontend = analyzer(&native, case);
    let fixture = Fixture::new(case, ORIGINAL);
    let wide = limits();
    let wide_input = request(&fixture.snapshot, &fixture.source, case, &wide);
    let required = native
        .required_syntax_fact_count(&wide_input.to_parse_request(), &deadline())
        .unwrap();
    let wide_artifact = frontend.prepare_artifact(&wide_input, &deadline()).unwrap();
    assert_eq!(
        wide_artifact
            .required_syntax_fact_count(&deadline())
            .unwrap(),
        required
    );
    let exact = limits_with_syntax_records(required);
    let exact_input = request(&fixture.snapshot, &fixture.source, case, &exact);
    let (result, artifact) = frontend
        .analyze_and_capture(
            &exact_input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    certified(result.document(), (1572, 1576), (1810, 1814));
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    assert_eq!(
        native
            .required_syntax_fact_count(&exact_input.to_parse_request(), &deadline())
            .unwrap(),
        required
    );
    let rebound = frontend
        .analyze_from_artifact(
            &exact_input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(rebound.document(), result.document());
}
