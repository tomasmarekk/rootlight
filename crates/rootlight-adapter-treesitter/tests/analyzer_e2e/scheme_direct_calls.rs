//! Exact source-backed Scheme direct calls through the public native analyzer.
//! Finite ordinary procedure wrappers preserve runtime gaps and refuse unproved donors.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/scheme_calls/mk-vicare.scm");
const SCHEME: LanguageCase = LanguageCase {
    name: "scheme",
    path: "src/direct-calls.scm",
    frontend: "tree-sitter-scheme-native-local-0.24.7-1",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(SCHEME, bytes);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(SCHEME),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, SCHEME),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn native_clean(source: &str) {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_scheme::LANGUAGE_SCHEME.into())
        .unwrap();
    let tree = parser.parse(source.as_bytes(), None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "clean native fixture: {source}"
    );
}

fn call_at(
    document: &rootlight_ir::NormalizedIrDocument,
    caller: &str,
    target: &str,
    start: u64,
    end: u64,
) {
    let named = |name: &str| {
        let values: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "scheme"
                    && entity.kind == EntityKind::Function
                    && entity.canonical_name == name
            })
            .collect();
        assert_eq!(values.len(), 1, "one own procedure {name}");
        values[0]
    };
    let caller = named(caller);
    let target = named(target);
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact direct call site");
    assert_eq!(sites[0].enclosing, Some(caller.id));
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    let calls: Vec<_> = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Calls
                && relation.subject == RelationEndpoint::Entity(caller.id)
                && relation.object == RelationEndpoint::Entity(target.id)
        })
        .collect();
    assert_eq!(calls.len(), 1, "one source-written typed Calls relation");
    let source = calls[0]
        .evidence
        .source
        .as_ref()
        .expect("direct call source evidence");
    assert_eq!(source, &sites[0].source);
    assert!(
        calls[0]
            .evidence
            .derivation
            .contains(&rootlight_ir::FactRef::Entity(caller.id))
    );
    assert!(
        calls[0]
            .evidence
            .derivation
            .contains(&rootlight_ir::FactRef::Fact(sites[0].id))
    );
    assert!(
        calls[0]
            .evidence
            .derivation
            .contains(&rootlight_ir::FactRef::Entity(target.id))
    );
    assert_ne!(
        document
            .coverage_records
            .iter()
            .find(|record| record.domain == FactDomain::Relations)
            .unwrap()
            .status,
        CoverageStatus::Complete
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Relations
                && gap.detail == "scheme-reader-library-and-runtime-binding-unavailable")
    );
}

#[test]
fn original_scheme_define_lambda_call_binds_its_later_source_procedure() {
    assert_eq!(ORIGINAL.len(), 5337);
    assert_eq!(&ORIGINAL[623..633], b"t:aux:size");
    native_clean(std::str::from_utf8(ORIGINAL).unwrap());
    let result = output(ORIGINAL, EncodingId::utf8());
    call_at(result.document(), "t:size", "t:aux:size", 623, 633);
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.content_hash() == content_hash(ORIGINAL))
    );
}

#[test]
fn ordinary_procedure_forms_forward_calls_and_direct_recursion_use_native_owners() {
    for (source, caller, target) in [
        (
            "(define caller (lambda (value) (later value)))\n(define later (lambda (arg) arg))\n",
            "caller",
            "later",
        ),
        (
            "(define (caller value) (later value))\n(define (later arg) arg)\n",
            "caller",
            "later",
        ),
        ("(define (again value) (again value))\n", "again", "again"),
    ] {
        let result = output(source.as_bytes(), EncodingId::utf8());
        let start = source.rfind(&format!("({target} value)")).unwrap() + 1;
        call_at(
            result.document(),
            caller,
            target,
            start as u64,
            (start + target.len()) as u64,
        );
    }
    for (source, operator) in [
        ("(define (caller) (later)) (define (later) #f)", "(later)"),
        (
            "(define (caller left right) (later left right)) (define (later first second) (cons first second))",
            "(later left right)",
        ),
    ] {
        native_clean(source);
        let result = output(source.as_bytes(), EncodingId::utf8());
        let start = source.find(operator).unwrap() + 1;
        call_at(
            result.document(),
            "caller",
            "later",
            start as u64,
            start as u64 + 5,
        );
    }
}

#[test]
fn local_shadows_aliases_duplicates_imports_and_dynamic_binding_are_not_call_proofs() {
    for source in [
        "(define (caller target) (target target)) (define (target arg) arg)",
        "(define (caller value) (let ((target value)) (target value))) (define (target arg) arg)",
        "(define (caller value) ((lambda (target) (target value)) value)) (define (target arg) arg)",
        "(define (caller value) (target value)) (define target other)",
        "(define (caller value) (target value)) (define (target arg) arg) (define (target arg) arg)",
        "(define (caller value value) (target value)) (define (target arg) arg)",
        "(define (caller value) (target value)) (define (target arg arg) arg)",
        "(define caller (lambda (value) (target value))) (define target (lambda (arg arg) arg))",
        "(define (caller value) (target value)) (define (target |arg|) arg)",
        "(define (caller value) (target value)) (define (target arg . rest) arg)",
        "(define-syntax template (syntax-rules () ((_ name) (define name hidden)))) (define (caller value) (target value)) (define (target arg) arg)",
        "(define (caller value) (let-syntax ((target (syntax-rules () ((_ arg) arg)))) (target value))) (define (target arg) arg)",
        "(define (caller value) (target value)) (define (target arg) arg) (define (mutate value) (set! target value))",
        "(define (caller value) (target value)) (define (target arg) arg) (define extra (eval code))",
        "(load file) (define (caller value) (target value)) (define (target arg) arg)",
        "(import (external)) (define (caller value) (target value)) (define (target arg) arg)",
        "(define lambda external) (define caller (lambda (value) (target value))) (define (target arg) arg)",
        "(define quote external) (define (caller value) (target value)) (define (target arg) arg) (define ignored (quote (set! target external)))",
        "(define (caller value) (target value)) (define (target arg) arg) (define (mutate quote) (quote (set! target external)))",
        "(define (caller value) (target value)) (define (target arg) arg) (define (mutate value) (let ((quote external)) (quote (set! target value))))",
        "(define (caller value) (target value)) (define (target arg) arg) (define (mutate value) (let quote ((value external)) (quote (set! target value))))",
        "(define (caller value) (target value)) (define (target arg) arg) (define (mutate value) (define quote external) (quote (set! target value)))",
        "(define (caller value) (target value)) (define (target arg) arg) (define (mutate \\x71;uote) (quote (set! target external)))",
        "(define (caller value) (target value)) (define (target arg) arg) (define ignored (do ((quote external)) (#t #f) (quote (set! target external))))",
        "(define (caller value) (target value)) (define (target arg) arg) (define ignored (case-lambda ((quote) (quote (set! target external)))))",
        "(define (caller value) (target value)) (define (target arg) arg) (define ignored (guard (quote (#t #f)) (quote (set! target external))))",
        "(define (caller value) (target value))",
    ] {
        native_clean(source);
        let result = output(source.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls),
            "{source}"
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
    }
    for template in [
        "(quasiquote ((unquote (set! target (lambda (arg) arg)))))",
        "\u{60}(,(set! target (lambda (arg) arg)))",
        "(quasiquote ((unquote-splicing (begin (set! target (lambda (arg) arg)) '()))))",
        "\u{60}(,@(begin (set! target (lambda (arg) arg)) '()))",
        "(quasisyntax ((unsyntax (set! target (lambda (arg) arg)))))",
        "#\u{60}(#,(set! target (lambda (arg) arg)))",
        "(quasisyntax ((unsyntax-splicing (begin (set! target (lambda (arg) arg)) '()))))",
        "#\u{60}(#,@(begin (set! target (lambda (arg) arg)) '()))",
        "#0=(begin (set! target (lambda (arg) arg)) 0)",
        "(begin '#0=(begin (set! target (lambda (arg) arg)) 0) #0#)",
    ] {
        let source = format!(
            "(define (caller value) (target value)) (define (target arg) arg) (define ignored {template})"
        );
        native_clean(&source);
        let result = output(source.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls),
            "{source}"
        );
    }
}

#[test]
fn implicit_record_names_are_not_unique_written_procedure_donors() {
    for target in ["holder", "make-holder", "holder?", "holder-key"] {
        let source = format!(
            "(define-record-type holder (fields key)) (define (caller value) ({target} value)) (define ({target} arg) arg)"
        );
        let result = output(source.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls),
            "{target}"
        );
    }
    let source = "(define-record-type holder (fields key)) (define (caller value) (later value)) (define (later arg) arg)";
    let result = output(source.as_bytes(), EncodingId::utf8());
    let start = source.find("(later value)").unwrap() + 1;
    call_at(
        result.document(),
        "caller",
        "later",
        start as u64,
        start as u64 + 5,
    );
}

#[test]
fn reader_changes_and_recovery_refuse_calls_but_quoted_fake_donors_stay_opaque() {
    for source in [
        "#!fold-case\n(define (caller value) (target value)) (define (target arg) arg)",
        "#!no-fold-case\n(define (caller value) (target value)) (define (target arg) arg)",
        "(define (caller value) (target value)) (define (target arg) arg) (broken",
    ] {
        let result = output(source.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls),
            "{source}"
        );
    }
    let source = "#;(define target other)\n#|(define target hidden)|#\n'(set! target changed)\n'(quasiquote ((unquote (set! target changed))))\n'(define (hidden quote) (quote (set! target changed)))\n'#0=(begin (set! target changed) #0#)\n(quote #1=(begin (set! target changed) #1#))\n(define (caller value) (target value)) (define (target arg) arg)";
    native_clean(source);
    let result = output(source.as_bytes(), EncodingId::utf8());
    let start = source.find("(target value)").unwrap() + 1;
    call_at(
        result.document(),
        "caller",
        "target",
        start as u64,
        start as u64 + 6,
    );
}

#[test]
fn encoded_scheme_calls_keep_physical_spans_and_current_source_identity() {
    let text = "; \u{394}\u{1f642}\n(define caller (lambda (value) (later value)))\n(define later (lambda (arg) arg))\n";
    let logical = text.find("(later value)").unwrap() + 1;
    for little in [true, false] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let encoding = EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap();
        let result = output(&bytes, encoding);
        let start = 2 + text[..logical].encode_utf16().count() * 2;
        call_at(
            result.document(),
            "caller",
            "later",
            start as u64,
            (start + 10) as u64,
        );
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::CallSite)
            .unwrap();
        assert_eq!(site.source.content_hash(), content_hash(&bytes));
        assert_eq!(site.syntactic_text_hash, content_hash(b"later"));
    }
}

#[test]
fn required_call_proof_and_relation_derivations_obey_exact_admission_limits() {
    let text = b"(define (caller value) (later value)) (define (later arg) arg)";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, SCHEME);
    let fixture = Fixture::new(SCHEME, text);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, SCHEME, &budget);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let exact = limits_with_syntax_records(required);
    let exact_request = request(&fixture.snapshot, &fixture.source, SCHEME, &exact);
    let (result, artifact) = analyzer
        .analyze_and_capture(
            &exact_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        required,
        artifact.required_syntax_fact_count(&deadline()).unwrap()
    );
    let start = std::str::from_utf8(text)
        .unwrap()
        .find("(later value)")
        .unwrap()
        + 1;
    call_at(
        result.document(),
        "caller",
        "later",
        start as u64,
        start as u64 + 5,
    );
    let short = limits_with_syntax_records(required - 1);
    let error = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, SCHEME, &short),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }) if observed == required && limit == required - 1),
        "{error:?}"
    );
    let mut ir = budget.ir().clone();
    ir.max_nested_items_per_record = 2;
    let small = AnalysisLimits::new(
        budget.max_source_bytes(),
        budget.max_syntax_nodes(),
        budget.max_syntax_depth(),
        budget.max_embedded_ranges(),
        budget.max_reported_memory_bytes(),
        budget.syntax_stream().clone(),
        budget.ir_stream().clone(),
        ir,
    )
    .unwrap();
    let error = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, SCHEME, &small),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(
            error,
            AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::NestedItems,
                observed: 3,
                limit: 2
            })
        ),
        "{error:?}"
    );
}

#[test]
fn scheme_call_artifacts_preserve_generation_and_refuse_changed_source_or_cancellation() {
    let text = "(define (caller value) (later value)) (define (later arg) arg)";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, SCHEME);
    let fixture = Fixture::new(SCHEME, text.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, SCHEME, &budget);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, SCHEME, &budget);
    let fresh = analyze(&analyzer, &next_request, &ExtensionSupport::default());
    let replay = analyzer
        .analyze_from_artifact(
            &next_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    assert_eq!(
        first
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>(),
        replay
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    );
    assert!(
        replay
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.generation() == next.source.generation())
    );
    let start = text.find("(later value)").unwrap() + 1;
    call_at(
        replay.document(),
        "caller",
        "later",
        start as u64,
        start as u64 + 5,
    );
    let edited = fixture.rewrite(text.replace("caller value", "caller later").as_bytes());
    let edited_request = request(&edited.snapshot, &edited.source, SCHEME, &budget);
    assert!(
        analyzer
            .analyze_from_artifact(
                &edited_request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
    let edited_output = analyze(&analyzer, &edited_request, &ExtensionSupport::default());
    assert!(
        !edited_output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        analyzer.prepare_artifact(&initial, &cancelled),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}
