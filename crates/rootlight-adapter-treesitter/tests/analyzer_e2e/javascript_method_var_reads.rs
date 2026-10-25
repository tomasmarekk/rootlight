//! Method-local JavaScript reads require both a variable and a callable owner.
//! The complete original keeps a same-named local in another method separate.

use super::*;
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
use rootlight_ir::{ContainerRef, NormalizedIrDocument, OccurrenceRecord};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/javascript_method_var_reads/curve.babel.js");

fn case() -> LanguageCase {
    *CASES.iter().find(|case| case.name == "javascript").unwrap()
}

fn output(source: &[u8]) -> AnalysisOutput {
    let native = Arc::new(provider());
    let case = case();
    let fixture = Fixture::new(case, source);
    let budget = limits();
    let result = analyze(
        &analyzer(&native, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn definition(
    document: &NormalizedIrDocument,
    source: &[u8],
    name: &str,
    kind: EntityKind,
    span: (u64, u64),
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == span.0
                && site.source.span().end_byte() == span.1
        })
        .collect();
    assert_eq!(sites.len(), 1, "written {name} definition at {span:?}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("written {name} definition resolves");
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.language, "javascript");
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(sites[0].source.content_hash(), content_hash(source));
    let evidence = sites[0].evidence.source.as_ref().unwrap();
    assert_eq!(Some(evidence), entity.evidence.source.as_ref());
    assert_eq!(evidence.content_hash(), content_hash(source));
    assert_eq!(evidence.span().file(), sites[0].source.span().file());
    assert!(evidence.span().start_byte() <= span.0);
    assert!(span.1 <= evidence.span().end_byte());
    symbol
}

fn curve_symbols(document: &NormalizedIrDocument) -> (SymbolId, SymbolId) {
    assert_eq!(ORIGINAL.len(), 1480);
    assert_eq!(&ORIGINAL[37..42], b"Curve");
    assert_eq!(&ORIGINAL[311..316], b"_draw");
    assert_eq!(&ORIGINAL[374..381], b"radiusX");
    assert_eq!(&ORIGINAL[511..518], b"radiusX");
    let class = definition(document, ORIGINAL, "Curve", EntityKind::Class, (37, 42));
    let owner = definition(document, ORIGINAL, "_draw", EntityKind::Method, (311, 316));
    let target = definition(
        document,
        ORIGINAL,
        "radiusX",
        EntityKind::Variable,
        (374, 381),
    );
    let entity = |symbol| {
        document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap()
    };
    assert_eq!(entity(owner).container, Some(ContainerRef::Entity(class)));
    assert_eq!(entity(target).container, Some(ContainerRef::Entity(owner)));
    (owner, target)
}

fn read<'document>(
    document: &'document NormalizedIrDocument,
    source: &[u8],
    owner: SymbolId,
    target: SymbolId,
    span: (u64, u64),
) -> &'document OccurrenceRecord {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == span.0
                && site.source.span().end_byte() == span.1
        })
        .collect();
    assert_eq!(sites.len(), 1, "one physical ordinary read at {span:?}");
    let site = sites[0];
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
    assert_eq!(
        site.enclosing,
        Some(owner),
        "initializer variables cannot donate a method owner"
    );
    assert_eq!(site.repository, document.repository);
    assert_eq!(site.generation, document.generation);
    assert_eq!(site.file, site.source.span().file());
    assert_eq!(site.source.content_hash(), content_hash(source));
    assert_eq!(site.evidence.source.as_ref(), Some(&site.source));
    let variable = document
        .entities
        .iter()
        .find(|entity| entity.id == target)
        .unwrap();
    assert_eq!(site.provenance, variable.provenance);
    let start = usize::try_from(span.0).unwrap();
    let end = usize::try_from(span.1).unwrap();
    assert_eq!(site.syntactic_text_hash, content_hash(&source[start..end]));
    assert_eq!(
        site.id,
        rootlight_ir::derive_occurrence_record_id(site).unwrap()
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(target)
                    && edge.evidence.source.as_ref() == Some(&site.source)
            })
            .count(),
        1
    );
    site
}

fn no_read(document: &NormalizedIrDocument, span: (u64, u64)) {
    assert!(document.occurrences.iter().all(|site| {
        site.role != OccurrenceRole::Read
            || site.source.span().start_byte() != span.0
            || site.source.span().end_byte() != span.1
            || !matches!(site.target, OccurrenceTarget::Resolved { .. })
    }));
}

#[test]
fn complete_original_curve_binary_read_resolves_its_method_local_variable() {
    let result = output(ORIGINAL);
    let (owner, target) = curve_symbols(result.document());
    read(result.document(), ORIGINAL, owner, target, (511, 518));
    assert!(result.document().relations.iter().all(|edge| {
        edge.predicate != RelationPredicate::Calls
            || edge.object != RelationEndpoint::Entity(target)
    }));
}

#[test]
fn same_spelling_locals_in_distinct_renamed_methods_never_share_targets() {
    let source = b"class Holder { first(){ var value = 1; var observed = value === this.value; } second(){ var value = 2; var observed = value === this.value; } }";
    let result = output(source);
    let document = result.document();
    let text = std::str::from_utf8(source).unwrap();
    let bindings: Vec<_> = text
        .match_indices("var value =")
        .map(|(at, _)| at + 4)
        .collect();
    let uses: Vec<_> = text
        .match_indices("= value ===")
        .map(|(at, _)| at + 2)
        .collect();
    assert_eq!((bindings.len(), uses.len()), (2, 2));
    assert_eq!(bindings, [28, 92]);
    assert_eq!(uses, [54, 118]);
    let mut targets = Vec::new();
    for (index, method) in ["first", "second"].into_iter().enumerate() {
        let start = u64::try_from(text.find(&format!("{method}()")).unwrap()).unwrap();
        let owner = definition(
            document,
            source,
            method,
            EntityKind::Method,
            (start, start + u64::try_from(method.len()).unwrap()),
        );
        let start = u64::try_from(bindings[index]).unwrap();
        let target = definition(
            document,
            source,
            "value",
            EntityKind::Variable,
            (start, start + 5),
        );
        let variable = document
            .entities
            .iter()
            .find(|entity| entity.id == target)
            .unwrap();
        assert_eq!(variable.container, Some(ContainerRef::Entity(owner)));
        let start = u64::try_from(uses[index]).unwrap();
        read(document, source, owner, target, (start, start + 5));
        targets.push(target);
    }
    assert_ne!(targets[0], targets[1]);
    for (at, _) in text.match_indices("this.value") {
        let start = u64::try_from(at + 5).unwrap();
        for target in &targets {
            assert!(
                document.occurrences.iter().all(|site| {
                    site.source.span().start_byte() != start
                        || site.source.span().end_byte() != start + 5
                        || site.target != (OccurrenceTarget::Resolved { symbol: *target })
                }),
                "a property selector is not a same-spelling lexical local"
            );
        }
    }
}

#[test]
fn unproved_dynamic_competing_and_nested_owners_do_not_receive_local_var_read_proofs() {
    for source in [
        "class Holder { run(){ var value=1; eval(code); return value === other; } }",
        "class Holder { run(){ var value=1; with(other){} return value === other; } }",
        "class Holder { run(){ var value=1; var value=2; return value === other; } }",
        "class Holder { run(){ var {value}=other; return value === other; } }",
        "class Holder { run(){ let value=1; return value === other; } }",
        "class Holder { run(){ var value=1; function inner(){ return value === other; } } }",
    ] {
        let result = output(source.as_bytes());
        let start = u64::try_from(source.find("return value ===").unwrap() + 7).unwrap();
        no_read(result.document(), (start, start + 5));
    }
}

#[test]
fn original_read_coordinates_follow_utf8_bom_and_physical_crlf() {
    let mut source = b"\xef\xbb\xbf".to_vec();
    for byte in ORIGINAL {
        if *byte == b'\n' {
            source.push(b'\r');
        }
        source.push(*byte);
    }
    let physical = |offset: usize| {
        u64::try_from(
            3 + offset
                + ORIGINAL[..offset]
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count(),
        )
        .unwrap()
    };
    let result = output(&source);
    let document = result.document();
    let owner = definition(
        document,
        &source,
        "_draw",
        EntityKind::Method,
        (physical(311), physical(316)),
    );
    let target = definition(
        document,
        &source,
        "radiusX",
        EntityKind::Variable,
        (physical(374), physical(381)),
    );
    read(
        document,
        &source,
        owner,
        target,
        (physical(511), physical(518)),
    );
}

#[test]
fn actual_raw_native_owner_and_target_witness_tampering_cannot_donate_read_authority() {
    let native = provider();
    let case = case();
    let fixture = Fixture::new(case, ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts: Vec<SyntaxFact>| {
        let parser = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(parser),
            producer_identity(),
            language(case),
            case.frontend,
            content_hash(BINARY_SEED),
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
    let (owner, target) = curve_symbols(baseline.document());
    read(baseline.document(), ORIGINAL, owner, target, (511, 518));
    for symbol in [owner, target] {
        let entity = baseline
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        let span = entity.evidence.source.as_ref().unwrap().span();
        let declarations: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::Declaration && fact.span() == span)
            .collect();
        assert_eq!(declarations.len(), 1);
        let declaration = declarations[0];
        let definitions: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str().ends_with(".definition")
                    && fact.parent() == Some(declaration.local_id())
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        for witness in [declaration, definitions[0]] {
            let mut facts = parsed.facts().to_vec();
            let next = facts
                .iter()
                .map(SyntaxFact::local_id)
                .max()
                .unwrap()
                .checked_add(1)
                .unwrap();
            facts.push(SyntaxFact::new(
                next,
                witness.parent(),
                witness.kind(),
                witness.span(),
                witness.depth(),
                witness.syntax_kind().clone(),
            ));
            no_read(replay(facts).document(), (511, 518));
        }
        let missing_definition = parsed
            .facts()
            .iter()
            .filter(|fact| fact.local_id() != definitions[0].local_id())
            .cloned()
            .collect();
        no_read(replay(missing_definition).document(), (511, 518));
    }
    let selected: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| {
            fact.kind() == SyntaxFactKind::Occurrence
                && fact.span().start_byte() == 511
                && fact.span().end_byte() == 518
                && fact.syntax_kind().as_str().ends_with(".reference")
        })
        .collect();
    assert_eq!(selected.len(), 1);
    let witness = selected[0];
    let mut facts = parsed.facts().to_vec();
    let next = facts
        .iter()
        .map(SyntaxFact::local_id)
        .max()
        .unwrap()
        .checked_add(1)
        .unwrap();
    facts.push(SyntaxFact::new(
        next,
        witness.parent(),
        witness.kind(),
        witness.span(),
        witness.depth(),
        witness.syntax_kind().clone(),
    ));
    no_read(replay(facts).document(), (511, 518));
}

#[test]
fn old_raw_facts_do_not_certify_same_width_current_dynamic_or_malformed_source() {
    let native = provider();
    let case = case();
    let fixture = Fixture::new(case, ORIGINAL);
    let budget = limits();
    let original = request(&fixture.snapshot, &fixture.source, case, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &original.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let start = ORIGINAL
        .windows(14)
        .position(|bytes| bytes == b"super._draw();")
        .unwrap();
    for replacement in [
        b"with(x){};    ".as_slice(),
        b"eval(code);   ".as_slice(),
        b"..............".as_slice(),
    ] {
        assert_eq!(replacement.len(), 14);
        let mut changed = ORIGINAL.to_vec();
        changed[start..start + 14].copy_from_slice(replacement);
        let current = fixture.rewrite(&changed);
        let parser = MockParseProvider::new(
            native.capabilities().clone(),
            parsed.facts().to_vec(),
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(parser),
            producer_identity(),
            language(case),
            case.frontend,
            content_hash(BINARY_SEED),
        )
        .unwrap();
        let result = execute_analysis(
            &frontend,
            &request(&current.snapshot, &current.source, case, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        no_read(result.document(), (511, 518));
    }
}

#[test]
fn exact_required_artifact_budget_replay_and_cancellation_keep_original_method_reads() {
    let native = Arc::new(provider());
    let case = case();
    let frontend = analyzer(&native, case);
    let fixture = Fixture::new(case, ORIGINAL);
    let budget = limits();
    let original = request(&fixture.snapshot, &fixture.source, case, &budget);
    let required = native
        .required_syntax_fact_count(&original.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 1);
    let exact = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, case, &exact);
    let (first, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let (owner, target) = curve_symbols(first.document());
    read(first.document(), ORIGINAL, owner, target, (511, 518));
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let next = fixture.next_generation();
    let input = request(&next.snapshot, &next.source, case, &exact);
    let fresh = analyze(&frontend, &input, &ExtensionSupport::default());
    let replay = frontend
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(replay.document(), fresh.document());
    assert_eq!(replay.report(), fresh.report());
    let (owner, target) = curve_symbols(replay.document());
    read(replay.document(), ORIGINAL, owner, target, (511, 518));
    let small = limits_with_syntax_records(required - 1);
    let rejected = frontend
        .prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, case, &small),
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(rejected, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }) if observed == required && limit == required - 1)
    );
    let cancel = Cancellation::new();
    assert!(cancel.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        frontend.analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel
        ),
        Err(AdapterError::Cancelled { .. })
    ));
    let changed = fixture.rewrite(&[ORIGINAL, b"\n"].concat());
    assert!(
        matches!(frontend.analyze_from_artifact(&request(&changed.snapshot, &changed.source, case, &exact), &artifact, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
}

#[test]
fn included_range_request_does_not_borrow_full_file_method_var_authority() {
    let native = Arc::new(provider());
    let case = case();
    let fixture = Fixture::new(case, ORIGINAL);
    let budget = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case),
        EncodingId::utf8(),
        vec![IncludedRange::new(fixture.source.span(), language(case))],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&native, case),
        &input,
        &ExtensionSupport::default(),
    );
    no_read(result.document(), (511, 518));
}
