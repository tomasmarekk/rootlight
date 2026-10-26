//! Ruby formal receivers require current source and exact same-method ownership.
//! The full original exercises an initializer owner without inventing dispatched
//! Calls, value facets, template binders or module/class runtime behavior.

use super::*;
use rootlight_adapter_sdk::{ParseOutput, SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
use rootlight_ir::ContainerRef;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/ruby_formal_receivers/html_helper.rb");
const LABEL: &str = "ruby.formal_receiver.reference";
const SIMPLE: &str = "module Written\n  def owned(value)\n    result = value.message\n  end\nend\n";

fn case() -> LanguageCase {
    LanguageCase {
        name: "ruby",
        path: "written.rb",
        frontend: "tree-sitter-ruby-0.23.1",
        source: "",
        generated: false,
        body_before: "",
        body_after: "",
    }
}

fn output(raw: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let native = Arc::new(provider());
    let fixture = Fixture::new(case(), raw);
    let bounds = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case()),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &bounds,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&native, case()),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    at: (u64, u64),
    kind: EntityKind,
    name: &str,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && (
                    site.source.span().start_byte(),
                    site.source.span().end_byte(),
                ) == at
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact {name} Definition");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("exact written Definition")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    symbol
}

fn contains(
    document: &rootlight_ir::NormalizedIrDocument,
    owner: SymbolId,
    member: SymbolId,
    at: (u64, u64),
) {
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == member)
        .unwrap();
    assert_eq!(entity.container, Some(ContainerRef::Entity(owner)));
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(owner)
                && edge.object == RelationEndpoint::Entity(member)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    let evidence = edges[0].evidence.source.as_ref().unwrap();
    assert!(evidence.span().start_byte() <= at.0 && evidence.span().end_byte() >= at.1);
}

fn reference(
    document: &rootlight_ir::NormalizedIrDocument,
    at: (u64, u64),
) -> &rootlight_ir::OccurrenceRecord {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && (
                    site.source.span().start_byte(),
                    site.source.span().end_byte(),
                ) == at
        })
        .collect();
    assert_eq!(sites.len(), 1);
    sites[0]
}

fn certified(
    document: &rootlight_ir::NormalizedIrDocument,
    name: (u64, u64),
    at: (u64, u64),
    owner: &str,
    written: &str,
) -> SymbolId {
    let parameter = definition(document, name, EntityKind::Parameter, written);
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == parameter)
        .unwrap();
    let ContainerRef::Entity(method) = entity.container.unwrap() else {
        panic!("method-owned formal")
    };
    let method_entity = document
        .entities
        .iter()
        .find(|entity| entity.id == method)
        .unwrap();
    assert_eq!(method_entity.kind, EntityKind::Method);
    assert_eq!(method_entity.canonical_name, owner);
    contains(document, method, parameter, name);
    let site = reference(document, at);
    assert_eq!(site.syntax_kind, LABEL);
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: parameter }
    );
    assert_eq!(site.enclosing, Some(method));
    assert_eq!(site.provenance, entity.provenance);
    assert_eq!(site.source.content_hash(), document.files[0].content_hash);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(parameter)
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
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(site.id))
    );
    parameter
}

#[test]
fn complete_original_formal_reference_keeps_namespace_method_and_initializer_ownership() {
    assert_eq!(ORIGINAL.len(), 1690);
    assert_eq!(
        ORIGINAL.windows(2).filter(|part| *part == b"\r\n").count(),
        36
    );
    assert_eq!(&ORIGINAL[1389..1390], b"e");
    assert_eq!(&ORIGINAL[1419..1420], b"e");
    let result = output(ORIGINAL, EncodingId::utf8());
    let document = result.document();
    let namespace = definition(document, (7, 20), EntityKind::Namespace, "HTMLValidator");
    let method = definition(
        document,
        (1372, 1388),
        EntityKind::Method,
        "whitelisted_tag?",
    );
    let reject = definition(document, (1507, 1520), EntityKind::Method, "reject_error?");
    contains(document, namespace, method, (1372, 1388));
    contains(document, namespace, reject, (1507, 1520));
    certified(
        document,
        (1389, 1390),
        (1419, 1420),
        "whitelisted_tag?",
        "e",
    );
    let initializer = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "match")
        .unwrap();
    assert_ne!(
        reference(document, (1419, 1420)).enclosing,
        Some(initializer.id)
    );
    for span in [(1419, 1428), (1419, 1455)] {
        assert!(
            document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite
                    && (
                        site.source.span().start_byte(),
                        site.source.span().end_byte()
                    ) == span)
        );
    }
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && (
                    site.source.span().start_byte(),
                    site.source.span().end_byte()
                ) == (1657, 1673))
    );
}

#[test]
fn equal_spelling_formals_bind_only_their_exact_written_method() {
    let text = "module Written\ndef first(slot)\nresult = slot.message\nend\ndef second(slot)\nresult = slot.message\nend\nend\n";
    let result = output(text.as_bytes(), EncodingId::utf8());
    let definitions: Vec<_> = text
        .match_indices("(slot)")
        .map(|(at, _)| u64::try_from(at + 1).unwrap())
        .collect();
    let uses: Vec<_> = text
        .match_indices("slot.message")
        .map(|(at, _)| u64::try_from(at).unwrap())
        .collect();
    assert_eq!(definitions.len(), 2);
    assert_eq!(uses.len(), 2);
    let first = certified(
        result.document(),
        (definitions[0], definitions[0] + 4),
        (uses[0], uses[0] + 4),
        "first",
        "slot",
    );
    let second = certified(
        result.document(),
        (definitions[1], definitions[1] + 4),
        (uses[1], uses[1] + 4),
        "second",
        "slot",
    );
    assert_ne!(first, second);
}

#[test]
fn unrelated_locals_and_equal_spelling_method_names_keep_their_old_reference_route() {
    let text = "def owned(value)\nresult = value.value\nresult.message\nend\n";
    let (fixture, bounds, parsed) = parse(text.as_bytes());
    let result = replay(&fixture, &bounds, &parsed, parsed.facts().to_vec());
    let local = u64::try_from(text.rfind("result.").unwrap()).unwrap();
    assert_eq!(
        reference(result.document(), (local, local + 6)).syntax_kind,
        "ruby.identifier.reference"
    );
    let property = u64::try_from(text.find(".value").unwrap() + 1).unwrap();
    assert_eq!(
        reference(result.document(), (property, property + 5)).syntax_kind,
        "ruby.identifier.reference"
    );
    let mut facts = parsed.facts().to_vec();
    let index = facts
        .iter()
        .position(|fact| {
            fact.syntax_kind().as_str() == "ruby.identifier.reference"
                && fact.span().start_byte() == property
                && fact.span().end_byte() == property + 5
        })
        .unwrap();
    let fact = facts[index].clone();
    facts[index] = SyntaxFact::new(
        fact.local_id(),
        fact.parent(),
        fact.kind(),
        fact.span(),
        fact.depth(),
        rootlight_adapter_sdk::SyntaxKindLabel::new(LABEL).unwrap(),
    );
    unproved(
        replay(&fixture, &bounds, &parsed, facts).document(),
        (property, property + 5),
    );
}

fn parse(raw: &[u8]) -> (Fixture, AnalysisLimits, ParseOutput) {
    let native = provider();
    let fixture = Fixture::new(case(), raw);
    let bounds = limits();
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &request(&fixture.snapshot, &fixture.source, case(), &bounds).to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    (fixture, bounds, parsed)
}

fn try_replay(
    fixture: &Fixture,
    bounds: &AnalysisLimits,
    parsed: &ParseOutput,
    facts: Vec<SyntaxFact>,
) -> Result<AnalysisOutput, AdapterError> {
    let native = provider();
    let parser = MockParseProvider::new(
        native.capabilities().clone(),
        facts,
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let frontend = TreeSitterAnalyzer::new(
        Arc::new(parser),
        producer_identity(),
        language(case()),
        case().frontend,
        content_hash(BINARY_SEED),
    )
    .unwrap();
    execute_analysis(
        &frontend,
        &request(&fixture.snapshot, &fixture.source, case(), bounds),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
}

fn replay(
    fixture: &Fixture,
    bounds: &AnalysisLimits,
    parsed: &ParseOutput,
    facts: Vec<SyntaxFact>,
) -> AnalysisOutput {
    try_replay(fixture, bounds, parsed, facts).unwrap()
}

fn unproved(document: &rootlight_ir::NormalizedIrDocument, at: (u64, u64)) {
    let site = reference(document, at);
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(site.id))
    );
}

#[test]
fn actual_native_witness_removal_duplication_and_parent_changes_refuse_adoption() {
    let (fixture, bounds, parsed) = parse(ORIGINAL);
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    certified(
        replay(&fixture, &bounds, &parsed, parsed.facts().to_vec()).document(),
        (1389, 1390),
        (1419, 1420),
        "whitelisted_tag?",
        "e",
    );
    let labels = [
        ("ruby.method.declaration", 1368, 1497),
        ("ruby.method.signature", 1368, 1391),
        ("ruby.parameter.declaration", 1389, 1390),
        ("ruby.identifier.definition", 1389, 1390),
        ("ruby.block.scope", 1397, 1491),
        ("ruby.variable.declaration", 1411, 1455),
    ];
    for (label, start, end) in labels {
        let selected: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == label
                    && (fact.span().start_byte(), fact.span().end_byte()) == (start, end)
            })
            .collect();
        assert_eq!(selected.len(), 1);
        let witness = selected[0];
        for variant in 0..3 {
            let mut facts = parsed.facts().to_vec();
            if variant == 0 {
                facts.retain(|fact| fact.local_id() != witness.local_id());
                for child in &mut facts {
                    if child.parent() == Some(witness.local_id()) {
                        *child = SyntaxFact::new(
                            child.local_id(),
                            witness.parent(),
                            child.kind(),
                            child.span(),
                            child.depth(),
                            child.syntax_kind().clone(),
                        );
                    }
                }
            } else if variant == 1 {
                let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
                facts.push(SyntaxFact::new(
                    next,
                    witness.parent(),
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    witness.syntax_kind().clone(),
                ));
            } else {
                let index = facts
                    .iter()
                    .position(|fact| fact.local_id() == witness.local_id())
                    .unwrap();
                facts[index] = SyntaxFact::new(
                    witness.local_id(),
                    None,
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    witness.syntax_kind().clone(),
                );
            }
            unproved(
                replay(&fixture, &bounds, &parsed, facts).document(),
                (1419, 1420),
            );
        }
    }
    let mut facts = parsed.facts().to_vec();
    facts.push(
        facts
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == LABEL && fact.span().start_byte() == 1419)
            .unwrap()
            .clone(),
    );
    // The public SDK coalesces identical records before lowering.
    assert_eq!(
        replay(&fixture, &bounds, &parsed, facts.clone()).document(),
        replay(&fixture, &bounds, &parsed, parsed.facts().to_vec()).document()
    );
    let duplicate = facts.last().unwrap().clone();
    let duplicate_id = duplicate.local_id();
    *facts.last_mut().unwrap() = SyntaxFact::new(
        duplicate_id,
        duplicate.parent(),
        duplicate.kind(),
        duplicate.span(),
        duplicate.depth(),
        rootlight_adapter_sdk::SyntaxKindLabel::new("ruby.unproven_formal_receiver.reference")
            .unwrap(),
    );
    assert!(matches!(try_replay(&fixture, &bounds, &parsed, facts),
        Err(AdapterError::Sink(SinkError::DuplicateSyntaxFact { local_id }))
            if local_id == duplicate_id));
}

#[test]
fn real_complete_unsupported_sources_cannot_accept_a_forced_positive_label() {
    let cases = [
        "def owned(value)\nvalue = other\nvalue.message\nend\n",
        "def owned(value)\nvalue += other\nvalue.message\nend\n",
        "def owned(value)\n[value].each { |value| value.message }\nend\n",
        "def owned(value)\ncase input\nin value\nend\nvalue.message\nend\n",
        "def owned(value)\neval('value = other')\nvalue.message\nend\n",
        "def owned(value = other)\nvalue.message\nend\n",
        "def owned(*value)\nvalue.message\nend\n",
        "def self.owned(value)\nvalue.message\nend\n",
        "def owned(value)\n/(?<value>a)/ =~ text\nvalue.message\nend\n",
        "def owned(value)\nvalue&.message\nend\n",
    ];
    for text in cases {
        let (fixture, bounds, parsed) = parse(text.as_bytes());
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete,
            "{text}"
        );
        let at = u64::try_from(text.rfind("value").unwrap()).unwrap();
        let mut facts = parsed.facts().to_vec();
        let selected: Vec<_> = facts
            .iter()
            .enumerate()
            .filter(|(_, fact)| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str().ends_with(".reference")
                    && fact.span().start_byte() == at
                    && fact.span().end_byte() == at + 5
            })
            .map(|(index, _)| index)
            .collect();
        assert_eq!(
            selected.len(),
            1,
            "real visible Reference prerequisite: {text}"
        );
        let fact = facts[selected[0]].clone();
        facts[selected[0]] = SyntaxFact::new(
            fact.local_id(),
            fact.parent(),
            fact.kind(),
            fact.span(),
            fact.depth(),
            rootlight_adapter_sdk::SyntaxKindLabel::new(LABEL).unwrap(),
        );
        unproved(
            replay(&fixture, &bounds, &parsed, facts).document(),
            (at, at + 5),
        );
    }
}

#[test]
fn same_width_changed_source_cannot_replay_an_old_formal_proof() {
    let (fixture, bounds, parsed) = parse(SIMPLE.as_bytes());
    let start = SIMPLE.find("value)").unwrap() as u64;
    let at = SIMPLE.rfind("value.").unwrap() as u64;
    certified(
        replay(&fixture, &bounds, &parsed, parsed.facts().to_vec()).document(),
        (start, start + 5),
        (at, at + 5),
        "owned",
        "value",
    );
    let changed = SIMPLE.replacen("value)", "other)", 1);
    let changed = fixture.rewrite(changed.as_bytes());
    unproved(
        replay(&changed, &bounds, &parsed, parsed.facts().to_vec()).document(),
        (at, at + 5),
    );
}

#[test]
fn exact_required_preflight_and_artifact_replay_preserve_formal_proof() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, case());
    let fixture = Fixture::new(case(), ORIGINAL);
    let wide = limits();
    let required = native
        .required_syntax_fact_count(
            &request(&fixture.snapshot, &fixture.source, case(), &wide).to_parse_request(),
            &deadline(),
        )
        .unwrap();
    let bounds = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, case(), &bounds);
    let (result, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    certified(
        result.document(),
        (1389, 1390),
        (1419, 1420),
        "whitelisted_tag?",
        "e",
    );
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let next = fixture.next_generation();
    let next_input = request(&next.snapshot, &next.source, case(), &bounds);
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
    assert!(
        matches!(frontend.prepare_artifact(&request(&fixture.snapshot, &fixture.source, case(), &low), &deadline()),
        Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }))
        if observed == required && limit == required - 1)
    );
}

#[test]
fn crlf_bom_and_utf16_orders_keep_original_physical_formal_spans() {
    let text = format!("# \u{03bb}\u{1f600}\r\n{}", SIMPLE.replace('\n', "\r\n"));
    let definition = text.find("value)").unwrap();
    let usage = text.rfind("value.").unwrap();
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let at = |index| u64::try_from(2 + 2 * text[..index].encode_utf16().count()).unwrap();
        let result = output(
            &raw,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        certified(
            result.document(),
            (at(definition), at(definition + 5)),
            (at(usage), at(usage + 5)),
            "owned",
            "value",
        );
    }
    let raw = format!("\u{feff}{text}");
    let result = output(raw.as_bytes(), EncodingId::utf8());
    certified(
        result.document(),
        ((definition + 3) as u64, (definition + 8) as u64),
        ((usage + 3) as u64, (usage + 8) as u64),
        "owned",
        "value",
    );
}

#[test]
fn selected_full_extent_and_partial_inputs_do_not_authorize_formal_targets() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, case());
    let fixture = Fixture::new(case(), ORIGINAL);
    let bounds = limits();
    for selected in [
        vec![(0, 1690)],
        vec![(1368, 1497)],
        vec![(0, 1366), (1368, 1690)],
    ] {
        let ranges = selected
            .into_iter()
            .map(|(start, end)| {
                IncludedRange::new(
                    SourceSpan::new(fixture.source.span().file(), start, end).unwrap(),
                    language(case()),
                )
            })
            .collect();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case()),
            EncodingId::utf8(),
            ranges,
            AnalysisTier::TierD,
            build_context(),
            &bounds,
        )
        .unwrap()
        .with_generated_status(false);
        let output = analyze(&frontend, &input, &ExtensionSupport::default());
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == LABEL
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
    }
}

fn with_ir(base: &AnalysisLimits, ir: IrLimits) -> AnalysisLimits {
    AnalysisLimits::new(
        base.max_source_bytes(),
        base.max_syntax_nodes(),
        base.max_syntax_depth(),
        base.max_embedded_ranges(),
        base.max_reported_memory_bytes(),
        base.syntax_stream().clone(),
        base.ir_stream().clone(),
        ir,
    )
    .unwrap()
}

#[test]
fn missing_long_method_owner_cannot_donate_its_formal_to_an_outer_entity() {
    let name = "m".repeat(300);
    let text = format!("module Written\ndef {name}(value)\nresult = value.message\nend\nend\n");
    let native = Arc::new(provider());
    let fixture = Fixture::new(case(), text.as_bytes());
    let base = limits();
    let mut ir = IrLimits::default();
    ir.max_string_bytes = 256;
    let bounds = with_ir(&base, ir);
    let result = analyze(
        &analyzer(&native, case()),
        &request(&fixture.snapshot, &fixture.source, case(), &bounds),
        &ExtensionSupport::default(),
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Method && entity.canonical_name == name)
    );
    let at = u64::try_from(text.rfind("value.").unwrap()).unwrap();
    unproved(result.document(), (at, at + 5));
}

#[test]
fn formal_reference_and_relation_use_real_exact_ir_admission_boundaries() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, case());
    let fixture = Fixture::new(case(), SIMPLE.as_bytes());
    let base = limits();
    let run = |ir| {
        let bounds = with_ir(&base, ir);
        frontend
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, case(), &bounds),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    let baseline = run(IrLimits::default()).unwrap();
    let name = u64::try_from(SIMPLE.find("value)").unwrap()).unwrap();
    let site = u64::try_from(SIMPLE.rfind("value.").unwrap()).unwrap();
    certified(
        baseline.document(),
        (name, name + 5),
        (site, site + 5),
        "owned",
        "value",
    );
    for relations in [false, true] {
        let defaults = IrLimits::default();
        let mut low = 0;
        let mut high = if relations {
            defaults.max_relations
        } else {
            defaults.max_total_records
        };
        let cap = |maximum| {
            let mut ir = IrLimits::default();
            if relations {
                ir.max_relations = maximum
            } else {
                ir.max_total_records = maximum
            }
            ir
        };
        while low < high {
            let middle = low + (high - low) / 2;
            match run(cap(middle)) {
                Ok(_) => high = middle,
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("unexpected failure: {error:?}"),
            }
        }
        assert!(low > 0);
        assert_eq!(run(cap(low)).unwrap().document(), baseline.document());
        assert!(matches!(
            run(cap(low - 1)),
            Err(AdapterError::Sink(SinkError::StreamLimit { .. }))
        ));
    }
}

#[test]
fn actual_outer_definition_name_geometry_cannot_be_replaced_by_comment_text() {
    let cases = [
        (
            "module Written\n# Altered\n  def owned(value)\n    result = value.message\n  end\nend\n",
            EntityKind::Namespace,
            "Written",
            "Altered",
        ),
        (
            "module Written\n# Written\n  def owned(value)\n    result = value.message\n  end\nend\n",
            EntityKind::Namespace,
            "Written",
            "Written",
        ),
        (
            "class Written\n# Altered\n  def owned(value)\n    result = value.message\n  end\nend\n",
            EntityKind::Class,
            "Written",
            "Altered",
        ),
        (
            "class Written\n# Written\n  def owned(value)\n    result = value.message\n  end\nend\n",
            EntityKind::Class,
            "Written",
            "Written",
        ),
        (
            "module Outer\n# Other\nclass Written\n# Altered\ndef owned(value)\nresult = value.message\nend\nend\nend\n",
            EntityKind::Namespace,
            "Outer",
            "Other",
        ),
        (
            "module Outer\n# Other\nclass Written\n# Altered\ndef owned(value)\nresult = value.message\nend\nend\nend\n",
            EntityKind::Class,
            "Written",
            "Altered",
        ),
    ];
    for (text, kind, written, replacement) in cases {
        let (fixture, bounds, parsed) = parse(text.as_bytes());
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete,
            "{text}"
        );
        let baseline = replay(&fixture, &bounds, &parsed, parsed.facts().to_vec());
        let declaration = u64::try_from(text.find("(value)").unwrap() + 1).unwrap();
        let site = u64::try_from(text.rfind("value.message").unwrap()).unwrap();
        certified(
            baseline.document(),
            (declaration, declaration + 5),
            (site, site + 5),
            "owned",
            "value",
        );
        let original_start = u64::try_from(text.find(written).unwrap()).unwrap();
        let width = u64::try_from(written.len()).unwrap();
        assert_eq!(written.len(), replacement.len());
        let owner = definition(
            baseline.document(),
            (original_start, original_start + width),
            kind,
            written,
        );
        let extent = baseline
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == owner)
            .unwrap()
            .evidence
            .source
            .as_ref()
            .unwrap()
            .span();
        let changed_start =
            u64::try_from(text.find(&format!("# {replacement}")).unwrap() + 2).unwrap();
        assert_ne!(original_start, changed_start);
        assert_eq!(
            &text.as_bytes()[usize::try_from(changed_start).unwrap()
                ..usize::try_from(changed_start + width).unwrap()],
            replacement.as_bytes()
        );
        let mut facts = parsed.facts().to_vec();
        let matching: Vec<_> = facts
            .iter()
            .enumerate()
            .filter(|(_, fact)| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str() == "ruby.identifier.definition"
                    && fact.span().start_byte() == original_start
                    && fact.span().end_byte() == original_start + width
            })
            .map(|(index, _)| index)
            .collect();
        assert_eq!(matching.len(), 1);
        let fact = facts[matching[0]].clone();
        facts[matching[0]] = SyntaxFact::new(
            fact.local_id(),
            fact.parent(),
            fact.kind(),
            SourceSpan::new(fact.span().file(), changed_start, changed_start + width).unwrap(),
            fact.depth(),
            fact.syntax_kind().clone(),
        );
        let forged = replay(&fixture, &bounds, &parsed, facts);
        assert!(
            forged
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == kind
                    && entity.canonical_name == replacement
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span() == extent)),
            "the generic owner still materializes; the fresh consumer must independently refuse it"
        );
        unproved(forged.document(), (site, site + 5));
    }
}
