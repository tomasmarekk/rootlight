//! Exact Prolog heads and bounded own-file calls retain independent source proofs.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, SinkError, StreamLimits,
    execute_analysis,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_adapters::PrologParseProvider;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, IrLimits,
    NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint,
    RelationPredicate, SourceRef, SourceSpan, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};

const ORIGINAL: &[u8] = include_bytes!("fixtures/prolog_head/age.pl");

fn analyze(
    raw: &[u8],
    ir: IrLimits,
    cancel: &Cancellation,
) -> Result<AnalysisOutput, AdapterError> {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("age.pl"), raw).unwrap();
    let repository = derive_repository(b"prolog-written-head-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(&RelativePath::parse(Path::new("age.pl")).unwrap(), 65_536)
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([45; 20]),
        SourceSpan::new(snapshot.file(), 0, raw.len() as u64).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(PrologParseProvider::new(65_536, 16_384).unwrap()),
        ProducerIdentity::new(
            "prolog-written-head",
            "1.0",
            content_hash(b"source-contract"),
        )
        .unwrap(),
        LanguageId::new("prolog").unwrap(),
        "prolog-written-head",
        PrologParseProvider::binary_digest(),
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
        65_536,
        16_384,
        128,
        1,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        ir,
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("prolog").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"prolog-written-build")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let result = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        cancel,
    )?;
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(result.document().files[0].byte_length, raw.len() as u64);
    assert_eq!(result.document().files[0].language, "prolog");
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    Ok(result)
}

fn definition(
    document: &NormalizedIrDocument,
    raw: &[u8],
    name: &str,
    start: u64,
    end: u64,
) -> SymbolId {
    let entities: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.language == "prolog"
                && entity.kind == EntityKind::Function
                && entity.canonical_name == name
        })
        .collect();
    assert_eq!(entities.len(), 1, "unique predicate indicator {name}");
    let entity = entities[0];
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
        })
        .collect();
    assert_eq!(sites.len(), 1, "one written Definition {name}");
    let site = sites[0];
    assert_eq!(
        site.source.span().start_byte(),
        start,
        "exact head start {name}"
    );
    assert_eq!(site.source.span().end_byte(), end, "exact head end {name}");
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(site.source.repository(), document.repository);
    assert_eq!(site.source.generation(), document.generation);
    assert_eq!(site.source.span().file(), document.files[0].id);
    assert_eq!(
        site.syntactic_text_hash,
        content_hash(&raw[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()])
    );
    assert!(
        document
            .provenance
            .iter()
            .any(|record| record.id == site.provenance)
    );
    entity.id
}

fn original_definitions(result: &AnalysisOutput) -> (SymbolId, SymbolId) {
    assert_eq!(ORIGINAL.len(), 353);
    assert_eq!(&ORIGINAL[177..191], b"ageAbove(S, A)");
    let source = std::str::from_utf8(ORIGINAL).unwrap();
    let birth = source.find("birthDay(patH, [1944, 8, 21])").unwrap();
    let simulation = source
        .find("simulation_date(simulation1, [2025, 1, 18])")
        .unwrap();
    let birth_id = definition(
        result.document(),
        ORIGINAL,
        "birthDay/2",
        birth as u64,
        (birth + "birthDay(patH, [1944, 8, 21])".len()) as u64,
    );
    definition(
        result.document(),
        ORIGINAL,
        "simulation_date/2",
        simulation as u64,
        (simulation + "simulation_date(simulation1, [2025, 1, 18])".len()) as u64,
    );
    let owner = definition(result.document(), ORIGINAL, "ageAbove/2", 177, 191);
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 200),
        "the body goal is not a clause definition"
    );
    assert!(
        result
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "prolog-body-semantics-partial")
    );
    (owner, birth_id)
}

#[test]
fn original_age_rule_definition_excludes_separator_whitespace() {
    let result = analyze(ORIGINAL, IrLimits::default(), &Cancellation::new()).unwrap();
    original_definitions(&result);
}

#[test]
fn head_definition_trims_only_trailing_trivia_with_unicode_bom_and_crlf() {
    let source = "\u{feff}% ghost(X).\r\nλ(α)\t\r\n :- next(α).\r\n'edge'(A) \t :- true.\r\nliteral('% fake(X).').\r\n";
    let result = analyze(source.as_bytes(), IrLimits::default(), &Cancellation::new()).unwrap();
    for (written, canonical) in [
        ("λ(α)", "λ/1"),
        ("'edge'(A)", "edge/1"),
        ("literal('% fake(X).')", "literal/1"),
    ] {
        let start = source.find(written).unwrap();
        definition(
            result.document(),
            source.as_bytes(),
            canonical,
            start as u64,
            (start + written.len()) as u64,
        );
    }
    assert!(!result.document().entities.iter().any(|entity| matches!(
        entity.canonical_name.as_str(),
        "ghost/1" | "fake/1" | "next/1"
    )));
    assert!(
        result
            .document()
            .relations
            .iter()
            .all(|edge| edge.predicate != RelationPredicate::Calls)
    );
}

#[test]
fn directives_dcg_qualified_operator_and_meta_goals_do_not_gain_static_calls() {
    for source in [
        ":- dynamic edge/1.\nedge(a).\nrun(X) :- edge(X).\n",
        "phrase(X) --> [X].\nrun(X) :- phrase(X).\n",
        "edge(a).\nrun(X) :- module:edge(X).\n",
        ":- op(500, xfx, link).\na link b.\n",
        "edge(a).\nrun(X) :- call(edge(X)).\n",
    ] {
        let output = analyze(source.as_bytes(), IrLimits::default(), &Cancellation::new()).unwrap();
        assert!(
            output
                .document()
                .relations
                .iter()
                .all(|edge| edge.predicate != RelationPredicate::Calls)
        );
        assert!(
            !output.document().skipped_regions.is_empty()
                || !output.document().diagnostics.is_empty()
        );
    }
}

#[test]
fn precancel_and_explicit_occurrence_limit_refuse_without_output() {
    let cancel = Cancellation::new();
    assert!(cancel.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        analyze(ORIGINAL, IrLimits::default(), &cancel),
        Err(AdapterError::Cancelled { .. })
    ));
    let mut low = IrLimits::default();
    low.max_occurrences = 0;
    assert!(matches!(
        analyze(ORIGINAL, low, &Cancellation::new()),
        Err(AdapterError::Sink(SinkError::StreamLimit { .. }))
    ));
}

#[test]
fn original_body_goal_call_remains_an_applicable_separate_requirement() {
    let output = analyze(ORIGINAL, IrLimits::default(), &Cancellation::new()).unwrap();
    let (owner, target) = original_definitions(&output);
    assert_eq!(&ORIGINAL[200..208], b"birthDay");
    let edges: Vec<_> = output
        .document()
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(owner)
                && edge.object == RelationEndpoint::Entity(target)
        })
        .collect();
    assert_eq!(
        edges.len(),
        1,
        "unchanged original applicable Calls requirement"
    );
    let source = edges[0].evidence.source.as_ref().unwrap();
    assert_eq!(source.content_hash(), content_hash(ORIGINAL));
    assert_eq!(source.span().file(), output.document().files[0].id);
    assert!(source.span().start_byte() <= 200 && source.span().end_byte() >= 208);
}
fn direct_edges(output: &AnalysisOutput) -> Vec<&rootlight_ir::RelationRecord> {
    output
        .document()
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect()
}

#[test]
fn ordinary_conjunction_binds_unique_written_indicators_and_retains_body_gaps() {
    let raw = b"target(a).\ntarget(b).\nready.\ncaller(X) :- target(X), ready.\n";
    let output = analyze(raw, IrLimits::default(), &Cancellation::new()).unwrap();
    assert_eq!(direct_edges(&output).len(), 2);
    let caller = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "caller/1")
        .unwrap();
    for edge in direct_edges(&output) {
        assert_eq!(edge.subject, RelationEndpoint::Entity(caller.id));
        let site = edge.evidence.source.as_ref().unwrap();
        assert_eq!(site.content_hash(), content_hash(raw));
        assert_eq!(site.generation(), output.document().generation);
        assert_eq!(site.repository(), output.document().repository);
        assert_eq!(site.span().file(), output.document().files[0].id);
        assert_eq!(edge.evidence.derivation.len(), 3);
    }
    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|item| item.code == "prolog-body-semantics-partial")
    );
}

#[test]
fn only_closed_numeric_comparisons_are_nonbinding_conjuncts() {
    for guard in ["X > 2", "(X-1)/12+(2*3) > -4", "X >= Y+3", "X =< Y+3"] {
        let source = format!("target(a). caller(X) :- target(X), {guard}.\n");
        let output = analyze(source.as_bytes(), IrLimits::default(), &Cancellation::new()).unwrap();
        assert_eq!(direct_edges(&output).len(), 1, "{source}");
    }
    for guard in [
        "X is 2",
        "X > mystery(X)",
        "X >< 2",
        "X > 2 junk",
        "X >",
        "X > 1; target(X)",
        "X <= 2",
        "X--Y > 0",
        "X > -+2",
        "X > 2**3",
    ] {
        let source = format!("target(a). caller(X) :- target(X), {guard}.\n");
        let output = analyze(source.as_bytes(), IrLimits::default(), &Cancellation::new()).unwrap();
        assert!(
            direct_edges(&output).is_empty(),
            "unproved operator remainder {source}"
        );
    }
}

#[test]
fn complete_sentence_census_withholds_mutation_reader_module_and_recovery_donors() {
    for barrier in [
        ":- module(sample, []).",
        ":- use_module(other).",
        ":- dynamic target/1.",
        ":- multifile target/1.",
        ":- meta_predicate caller(0).",
        ":- op(500, xfx, custom).",
        "term_expansion(X, X).",
        "goal_expansion(X, X).",
        "bad ++ term.",
        "other(X) :+ target(X).",
        "phrase(X) --> [X].",
        "?- target(a).",
        "broken(",
        "caller2(X) :- call(target(X)).",
        "caller2(X) :- assertz(target(X)).",
        "caller2(X) :- module:target(X).",
        "caller2(X) :- (target(X); target(X)).",
        "harmless(X) :--1 > 0.",
        "true :+-1 > 0.",
        "\"bad\" target(a).",
        "`bad` target(a).",
        "target(a \"bad\").",
        "target(a `bad`).",
        "\"bad\"",
        "`bad`",
    ] {
        let source = format!("target(a). caller(X) :- target(X). {barrier}\n");
        let output = analyze(source.as_bytes(), IrLimits::default(), &Cancellation::new()).unwrap();
        assert!(
            direct_edges(&output).is_empty(),
            "complete-source barrier {barrier}"
        );
        assert_eq!(
            output.document().files[0].content_hash,
            content_hash(source.as_bytes())
        );
        assert!(!output.document().diagnostics.is_empty());
    }
}

#[test]
fn duplicate_quoted_competitors_wrong_arity_and_forward_targets_do_not_bind_by_spelling() {
    for source in [
        "target(a). gap. target(b). caller(X) :- target(X).",
        "target(a). 'target'(b). caller(X) :- target(X).",
        "target(a,b). caller(X) :- target(X).",
        "caller(X) :- target(X). target(a).",
        "caller(X) :- foreign(X).",
    ] {
        let output = analyze(source.as_bytes(), IrLimits::default(), &Cancellation::new()).unwrap();
        assert!(direct_edges(&output).is_empty(), "{source}");
    }
}

#[test]
fn data_arguments_comments_and_literals_do_not_become_executable_goals() {
    let raw = b"target(a). data(a). caller(X) :- target(data(a)), data(X). % target(X).\n";
    let output = analyze(raw, IrLimits::default(), &Cancellation::new()).unwrap();
    assert_eq!(direct_edges(&output).len(), 2);
    let sites: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(sites.len(), 2);
    assert!(
        sites
            .iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    for source in [
        "target(a). caller(X) :- target(\"target(X)\").",
        "target(a). caller(X) :- target(X +).",
        "target(a). caller(X) :- target((X; call(X))).",
        "target(a). caller(X) :- target([X | rest]).",
        "target (a). caller(X) :- target(X).",
        "target(a). caller(X) :- target (X).",
        "target(a). caller(X) :- target(data (X)).",
        "target(a). caller(X) :- target(data/* gap */(X)).",
    ] {
        let output = analyze(source.as_bytes(), IrLimits::default(), &Cancellation::new()).unwrap();
        assert!(
            direct_edges(&output).is_empty(),
            "unproved data grammar {source}"
        );
    }
}

#[test]
fn closed_true_premise_query_is_distinguished_from_unproved_bottom_up_declarations() {
    let base = "target(a). caller(X) :- target(X). ";
    let positive = format!("{base}true :+ caller(a).\n");
    let output = analyze(
        positive.as_bytes(),
        IrLimits::default(),
        &Cancellation::new(),
    )
    .unwrap();
    assert_eq!(direct_edges(&output).len(), 1);
    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|item| item.code == "prolog-sentence-not-analyzed")
    );
    for suffix in [
        "false :+ caller(a).",
        "true :+ call(caller(a)).",
        "true :+ module:caller(a).",
        "true :+ assertz(target(b)).",
        "true :+ caller(a); target(a).",
        "true :+ .",
    ] {
        let source = format!("{base}{suffix}\n");
        let output = analyze(source.as_bytes(), IrLimits::default(), &Cancellation::new()).unwrap();
        assert!(direct_edges(&output).is_empty(), "query boundary {suffix}");
    }
}

#[test]
fn direct_call_physical_spans_survive_bom_unicode_prefix_and_crlf() {
    let source = "\u{feff}% λ target(X).\r\ntarget(a).\r\ncaller(X) :- target(X).\r\n";
    let output = analyze(source.as_bytes(), IrLimits::default(), &Cancellation::new()).unwrap();
    let edges = direct_edges(&output);
    assert_eq!(edges.len(), 1);
    let expected = source.rfind("target(X)").unwrap();
    let span = edges[0].evidence.source.as_ref().unwrap().span();
    assert_eq!(span.start_byte(), u64::try_from(expected).unwrap());
    assert_eq!(span.end_byte(), u64::try_from(expected + 6).unwrap());
    let start = usize::try_from(span.start_byte()).unwrap();
    let end = usize::try_from(span.end_byte()).unwrap();
    assert_eq!(&source.as_bytes()[start..end], b"target");
}

#[test]
fn public_call_proof_obeys_relation_record_and_nested_item_boundaries() {
    let raw = b"target(a). caller(X) :- target(X).\n";
    let run = |mut ir: IrLimits| {
        ir.max_extensions = 3;
        analyze(raw, ir, &Cancellation::new())
    };
    let baseline = run(IrLimits::default()).unwrap();
    assert_eq!(direct_edges(&baseline).len(), 1);
    assert_eq!(baseline.document().entities.len(), 2);
    let usage =
        rootlight_ir::IrDocumentUsage::measure(baseline.document(), &IrLimits::default()).unwrap();
    let mut no_calls = baseline.document().clone();
    no_calls
        .relations
        .retain(|edge| edge.predicate != RelationPredicate::Calls);
    let prior = rootlight_ir::IrDocumentUsage::measure(&no_calls, &IrLimits::default()).unwrap();
    assert_eq!(usage.total_nested_items(), prior.total_nested_items() + 3);
    for boundary in 0..3 {
        for one_less in [false, true] {
            let mut limits = IrLimits::default();
            let exact = match boundary {
                0 => baseline.document().relations.len(),
                1 => usage.total_nested_items(),
                2 => 3,
                _ => unreachable!(),
            };
            let ceiling = exact - usize::from(one_less);
            match boundary {
                0 => limits.max_relations = ceiling,
                1 => limits.max_total_nested_items = ceiling,
                2 => limits.max_nested_items_per_record = ceiling,
                _ => unreachable!(),
            }
            match (one_less, run(limits)) {
                (false, Ok(actual)) => assert_eq!(actual.document(), baseline.document()),
                (true, Err(AdapterError::Sink(SinkError::StreamLimit { .. }))) => {}
                other => panic!("wrong proof admission at boundary {boundary}: {other:?}"),
            }
        }
    }
    let mut low = IrLimits::default();
    low.max_total_records = usage.total_records() - 1;
    assert!(matches!(
        run(low),
        Err(AdapterError::Sink(SinkError::StreamLimit { .. }))
    ));
}
struct ProofReplay {
    inner: PrologParseProvider,
    mode: usize,
}

impl rootlight_adapter_sdk::ParseProvider for ProofReplay {
    fn capabilities(&self) -> &rootlight_adapter_sdk::ParseCapabilities {
        rootlight_adapter_sdk::ParseProvider::capabilities(&self.inner)
    }

    fn parse(
        &self,
        request: &rootlight_adapter_sdk::ParseRequest<'_>,
        sink: &mut dyn rootlight_adapter_sdk::SyntaxFactSink,
        cancel: &Cancellation,
    ) -> Result<rootlight_adapter_sdk::ParseReport, AdapterError> {
        use rootlight_adapter_sdk::{
            ResourceUsage, StreamEnd, SyntaxFact, SyntaxFactBatch, execute_parse,
        };
        let parsed = execute_parse(
            &self.inner,
            request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancel,
        )?;
        let mut facts = parsed.facts().to_vec();
        let selected = if self.mode == 0 || self.mode == 2 {
            "prolog.goal_target.signature"
        } else {
            "prolog.goal_context.signature"
        };
        let index = facts
            .iter()
            .position(|fact| fact.syntax_kind().as_str() == selected)
            .expect("actual producer supplies the required proof being corrupted");
        let fact = facts[index].clone();
        if self.mode < 2 {
            facts.remove(index);
        } else if self.mode == 2 || self.mode == 3 {
            let id = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
            facts.push(SyntaxFact::new(
                id,
                fact.parent(),
                fact.kind(),
                fact.span(),
                fact.depth(),
                fact.syntax_kind().clone(),
            ));
        } else {
            let span = fact.span();
            facts[index] = SyntaxFact::new(
                fact.local_id(),
                fact.parent(),
                fact.kind(),
                SourceSpan::new(span.file(), span.start_byte() + 1, span.end_byte()).unwrap(),
                fact.depth(),
                fact.syntax_kind().clone(),
            );
        }
        for chunk in facts.chunks(32) {
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), chunk.to_vec(), Vec::new()),
                cancel,
            )?;
        }
        if !parsed.diagnostics().is_empty() {
            sink.push_cancellable(
                SyntaxFactBatch::new(
                    sink.next_sequence(),
                    Vec::new(),
                    parsed.diagnostics().to_vec(),
                ),
                cancel,
            )?;
        }
        let usage = sink.staged_usage();
        Ok(rootlight_adapter_sdk::ParseReport::new(
            parsed.report().coverage().clone(),
            ResourceUsage::new(
                request.source().bytes().len(),
                facts.len(),
                facts.len(),
                2,
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

#[test]
fn missing_duplicate_and_wrong_source_contexts_cannot_resolve_an_exact_spelling() {
    let raw = b"target(a). caller(X) :- target(X).\n";
    assert_eq!(
        direct_edges(&analyze(raw, IrLimits::default(), &Cancellation::new()).unwrap()).len(),
        1
    );
    for mode in 0..5 {
        let output = analyze_replay(raw, IrLimits::default(), &Cancellation::new(), mode).unwrap();
        assert!(
            direct_edges(&output).is_empty(),
            "corrupted context mode {mode}"
        );
        let sites: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .collect();
        assert_eq!(sites.len(), 1);
        assert!(matches!(
            sites[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "prolog-own-file-direct-goal-binding-unavailable")
        );
    }
}
fn analyze_replay(
    raw: &[u8],
    ir: IrLimits,
    cancel: &Cancellation,
    mode: usize,
) -> Result<AnalysisOutput, AdapterError> {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("age.pl"), raw).unwrap();
    let repository = derive_repository(b"prolog-written-head-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(&RelativePath::parse(Path::new("age.pl")).unwrap(), 65_536)
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([45; 20]),
        SourceSpan::new(snapshot.file(), 0, raw.len() as u64).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(ProofReplay {
            inner: PrologParseProvider::new(65_536, 16_384).unwrap(),
            mode,
        }),
        ProducerIdentity::new(
            "prolog-written-head",
            "1.0",
            content_hash(b"source-contract"),
        )
        .unwrap(),
        LanguageId::new("prolog").unwrap(),
        "prolog-written-head",
        PrologParseProvider::binary_digest(),
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
        65_536,
        16_384,
        128,
        1,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        ir,
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("prolog").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"prolog-written-build")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let result = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        cancel,
    )?;
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(result.document().files[0].byte_length, raw.len() as u64);
    assert_eq!(result.document().files[0].language, "prolog");
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    Ok(result)
}
