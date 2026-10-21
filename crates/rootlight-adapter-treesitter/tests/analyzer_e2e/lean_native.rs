//! Complete original written function, direct formals and own-body Read.
use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/lean/conv_eval.lean");
const LEAN: LanguageCase = LanguageCase {
    name: "lean",
    path: "written.lean",
    frontend: "lean-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
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
    assert_eq!(sites.len(), 1, "one exact {kind:?} {name}@{start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("the written definition resolves to its own entity")
    };
    let entity = document
        .entities
        .iter()
        .find(|item| item.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    symbol
}

#[test]
fn complete_original_direct_formals_and_body_read_have_exact_own_targets() {
    assert_eq!(ORIGINAL.len(), 4386);
    for (start, end, text) in [
        (254, 257, b"the".as_slice()),
        (259, 260, b"A"),
        (272, 273, b"x"),
        (282, 283, b"x"),
    ] {
        assert_eq!(&ORIGINAL[start..end], text);
    }
    let fixture = Fixture::new(LEAN, ORIGINAL);
    let provider = Arc::new(provider());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, LEAN, &budget);
    let output = analyze(
        &analyzer(&provider, LEAN),
        &request,
        &ExtensionSupport::default(),
    );
    let document = output.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].byte_length, 4386);
    assert_eq!(document.files[0].content_hash, content_hash(ORIGINAL));
    let owner = definition(document, "the", EntityKind::Function, 254, 257);
    let a = definition(document, "A", EntityKind::Parameter, 259, 260);
    let x = definition(document, "x", EntityKind::Parameter, 272, 273);
    for (member, start, end) in [(a, 259, 260), (x, 272, 273)] {
        let entity = document
            .entities
            .iter()
            .find(|item| item.id == member)
            .unwrap();
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::Entity(owner))
        );
        let relations: Vec<_> = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner)
                    && relation.object == RelationEndpoint::Entity(member)
            })
            .collect();
        assert_eq!(relations.len(), 1);
        let source = relations[0].evidence.source.as_ref().unwrap();
        assert!(source.span().start_byte() <= start && source.span().end_byte() >= end);
        assert_eq!(source.content_hash(), fixture.source.content_hash());
        assert_eq!(source.repository(), fixture.source.repository());
        assert_eq!(source.generation(), fixture.source.generation());
        assert_eq!(source.span().file(), fixture.source.span().file());
    }
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 282
                && site.source.span().end_byte() == 283
        })
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].target, OccurrenceTarget::Resolved { symbol: x });
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 282
            && site.source.span().end_byte() == 283
    }));
    for site in &document.occurrences {
        assert_eq!(site.source.content_hash(), fixture.source.content_hash());
        assert_eq!(site.source.repository(), fixture.source.repository());
        assert_eq!(site.source.generation(), fixture.source.generation());
        assert_eq!(site.source.span().file(), fixture.source.span().file());
        assert!(site.source.span().end_byte() <= 4386);
    }
}

fn lean_output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let fixture = Fixture::new(LEAN, bytes);
    let native = Arc::new(provider());
    let budget = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(LEAN),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let output = analyze(
        &analyzer(&native, LEAN),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(output.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(output.document().files[0].content_hash, content_hash(bytes));
    for site in &output.document().occurrences {
        assert_eq!(site.source.content_hash(), fixture.source.content_hash());
        assert_eq!(site.source.repository(), fixture.source.repository());
        assert_eq!(site.source.generation(), fixture.source.generation());
        assert!(site.source.span().end_byte() <= u64::try_from(bytes.len()).unwrap());
    }
    output
}

fn lean_read(
    document: &rootlight_ir::NormalizedIrDocument,
    start: u64,
    end: u64,
) -> &rootlight_ir::OccurrenceRecord {
    let selected: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(selected.len(), 1);
    selected[0]
}

#[test]
fn renamed_direct_formals_resolve_while_nested_qualified_and_ambiguous_uses_remain_unproved() {
    for source in [
        "def owned (value : Nat) := value\n",
        "def renamed (argument : Nat) := argument\n",
    ] {
        let output = lean_output(source.as_bytes(), EncodingId::utf8());
        let name = if source.starts_with("def owned") {
            "value"
        } else {
            "argument"
        };
        let formal = u64::try_from(source.find(name).unwrap()).unwrap();
        let used = u64::try_from(source.rfind(name).unwrap()).unwrap();
        let symbol = definition(
            output.document(),
            name,
            EntityKind::Parameter,
            formal,
            formal + name.len() as u64,
        );
        assert_eq!(
            lean_read(output.document(), used, used + name.len() as u64).target,
            OccurrenceTarget::Resolved { symbol }
        );
    }
    for source in [
        "def owned (value : Nat) := let value := 0; value\n",
        "def owned (value : Nat) := (fun value => value) value\n",
        "def owned (value : Nat) := Other.value\n",
        "def owned (value : Nat) := «value»\n",
        "def owned (value value : Nat) := value\n",
        "def owned (value : Nat) := value\ndef owned (value : Nat) := value\n",
    ] {
        let output = lean_output(source.as_bytes(), EncodingId::utf8());
        assert!(
            output
                .document()
                .occurrences
                .iter()
                .filter(|site| matches!(
                    site.role,
                    OccurrenceRole::Read | OccurrenceRole::Reference
                ))
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
            "{source}"
        );
        assert!(
            output.document().relations.iter().all(|edge| !matches!(
                edge.predicate,
                RelationPredicate::RefersTo | RelationPredicate::Calls
            )),
            "{source}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail.contains("lean-")),
            "{source}"
        );
    }
}

#[test]
fn original_bom_and_both_utf16_encodings_keep_exact_physical_definitions_and_read() {
    let plain = std::str::from_utf8(ORIGINAL).unwrap();
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
        let output = lean_output(&bytes, EncodingId::new(encoding).unwrap());
        let position = |offset| u64::try_from(encode(&plain[..offset]).len()).unwrap();
        definition(
            output.document(),
            "the",
            EntityKind::Function,
            position(254),
            position(257),
        );
        definition(
            output.document(),
            "A",
            EntityKind::Parameter,
            position(259),
            position(260),
        );
        let x = definition(
            output.document(),
            "x",
            EntityKind::Parameter,
            position(272),
            position(273),
        );
        assert_eq!(
            lean_read(output.document(), position(282), position(283)).target,
            OccurrenceTarget::Resolved { symbol: x }
        );
        assert_eq!(output.document().files[0].byte_length, bytes.len() as u64);
    }
}

#[test]
fn fresh_required_artifact_exact_and_one_less_keep_original_binding_and_generation() {
    let fixture = Fixture::new(LEAN, ORIGINAL);
    let native = Arc::new(provider());
    let frontend = analyzer(&native, LEAN);
    let wide = limits();
    let input = request(&fixture.snapshot, &fixture.source, LEAN, &wide);
    let required = native
        .required_syntax_fact_count(&input.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 6);
    let exact = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, LEAN, &exact);
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
    let x = definition(output.document(), "x", EntityKind::Parameter, 272, 273);
    assert_eq!(
        lean_read(output.document(), 282, 283).target,
        OccurrenceTarget::Resolved { symbol: x }
    );
    let next = fixture.next_generation();
    let input = request(&next.snapshot, &next.source, LEAN, &exact);
    let replay = frontend
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        replay.document(),
        analyze(&frontend, &input, &ExtensionSupport::default()).document()
    );
    assert!(
        replay
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.generation() == next.source.generation())
    );
    let below = limits_with_syntax_records(required - 1);
    let input = request(&fixture.snapshot, &fixture.source, LEAN, &below);
    assert_eq!(
        native
            .required_syntax_fact_count(&input.to_parse_request(), &deadline())
            .unwrap(),
        required
    );
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
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit })
            if observed == required && limit == required - 1),
            "{error:?}"
        );
    }
    let changed = fixture.rewrite(&[ORIGINAL, b" "].concat());
    assert!(
        matches!(frontend.analyze_from_artifact(&request(&changed.snapshot, &changed.source, LEAN, &wide), &artifact,
        ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()).unwrap_err(),
        AdapterError::ProviderFailed { code } if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        frontend.prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, LEAN, &wide),
            &cancelled
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn actual_materialization_refuses_missing_and_competing_raw_witnesses_after_baseline_binding() {
    use rootlight_adapter_sdk::{
        SyntaxFact, SyntaxFactKind, SyntaxKindLabel, testkit::MockParseProvider,
    };
    let native = provider();
    let fixture = Fixture::new(LEAN, ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, LEAN, &budget);
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
            producer_identity(),
            language(LEAN),
            LEAN.frontend,
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
    assert!(matches!(
        lean_read(baseline.document(), 282, 283).target,
        OccurrenceTarget::Resolved { .. }
    ));
    for (label, start, end) in [
        ("lean.formal_binding_context.signature", 250, 283),
        ("lean.formal_body_target.signature", 271, 283),
        ("lean.function.declaration", 250, 283),
        ("lean.parameter.declaration", 271, 278),
        ("lean.name.definition", 254, 257),
        ("lean.name.definition", 272, 273),
    ] {
        for operation in [0, 1, 2] {
            let mut facts = parsed.facts().to_vec();
            let selected: Vec<_> = facts
                .iter()
                .filter(|fact| {
                    fact.syntax_kind().as_str() == label
                        && fact.span().start_byte() == start
                        && fact.span().end_byte() == end
                })
                .cloned()
                .collect();
            assert_eq!(
                selected.len(),
                1,
                "one exact baseline witness {label}:{start}..{end}"
            );
            let witness = &selected[0];
            if operation == 0 {
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
            } else {
                let syntax = if operation == 1 {
                    witness.syntax_kind().clone()
                } else {
                    SyntaxKindLabel::new(match witness.kind() {
                        SyntaxFactKind::Declaration => "lean.competing.declaration",
                        SyntaxFactKind::Signature => "lean.competing.signature",
                        _ => "lean.competing.definition",
                    })
                    .unwrap()
                };
                facts.push(SyntaxFact::new(
                    facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
                    witness.parent(),
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    syntax,
                ));
            }
            let output = replay(facts);
            assert!(
                matches!(
                    lean_read(output.document(), 282, 283).target,
                    OccurrenceTarget::Unresolved { .. }
                ),
                "{label} operation={operation}"
            );
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail
                        == "lean-source-owned-direct-formal-body-binding-not-certified")
            );
        }
    }
}
