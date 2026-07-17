//! End-to-end contracts for parser-independent Tree-sitter lowering.
//!
//! Fake syntax providers exercise stable IDs, conservative relations, evidence,
//! coverage gaps, cancellation, and canonical normalized-IR decoding.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds,
    CoverageReport, DiagnosticCode, EncodingId, GenerationBoundSnapshot, IncludedRange,
    LanguageAnalyzer, LanguageId, MemoryAdmissionPolicy, MemoryEnforcement, ParseCapabilities,
    RequestError, SinkError, StreamLimits, SyntaxFact, SyntaxFactKind, SyntaxKindLabel,
    execute_analysis, testkit::MockParseProvider,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, DiagnosticSeverity, EntityFlag,
    ExtensionSupport, FactEvidence, IrDocument, IrLimits, LexicalEvidenceKind, OccurrenceRole,
    ProducerIdentity, RelationPredicate, SourceRef, SourceSpan, decode_ir_document,
    decode_lexical_evidence_envelope,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const SOURCE: &str =
    "mod api {\n    /// docs for alpha\n    pub fn alpha() { beta(); }\n    use crate::dep;\n}\n";

#[derive(Clone, Copy)]
struct LocalIds {
    root: u64,
    module: u64,
    comment: u64,
    function: u64,
    call: u64,
    import: u64,
}

#[test]
fn lowering_is_independent_of_local_ids_and_emission_order() {
    let (_temporary, snapshot, source) = source_fixture();
    let analysis_limits = limits(IrLimits::default());
    let first = analyze(
        &snapshot,
        &source,
        &analysis_limits,
        facts(
            &source,
            LocalIds {
                root: 10,
                module: 20,
                comment: 30,
                function: 40,
                call: 50,
                import: 60,
            },
            false,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("first lowering commits");
    let second = analyze(
        &snapshot,
        &source,
        &analysis_limits,
        facts(
            &source,
            LocalIds {
                root: 601,
                module: 509,
                comment: 407,
                function: 307,
                call: 211,
                import: 101,
            },
            true,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("reordered lowering commits");

    assert_eq!(first.document(), second.document());
    assert_eq!(first.report().coverage(), second.report().coverage());
}

#[test]
fn duplicate_captures_deduplicate_without_reclassifying_definitions() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let ids = LocalIds {
        root: 1,
        module: 2,
        comment: 3,
        function: 4,
        call: 5,
        import: 6,
    };
    let mut captured = facts(&source, ids, false);
    captured.push(SyntaxFact::new(
        70,
        Some(ids.function),
        SyntaxFactKind::Occurrence,
        span_for_nth(&source, "alpha", 1),
        3,
        label("rust.function.definition"),
    ));
    captured.push(SyntaxFact::new(
        71,
        Some(ids.function.wrapping_add(4_000_000)),
        SyntaxFactKind::Occurrence,
        span_for(&source, "beta()"),
        4,
        label("rust.call.reference"),
    ));

    let output = analyze(
        &snapshot,
        &source,
        &limits,
        captured.clone(),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("duplicate semantic captures lower deterministically");
    assert_eq!(output.document().entities.len(), 2);
    assert_eq!(
        output
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
            .count(),
        2
    );
    assert_eq!(
        output
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .count(),
        1
    );
}

#[test]
fn ambiguous_definition_capture_becomes_an_explicit_gap() {
    let (_temporary, snapshot, source) = source_fixture();
    let default_limits = limits(IrLimits::default());
    let ids = LocalIds {
        root: 1,
        module: 2,
        comment: 3,
        function: 4,
        call: 5,
        import: 6,
    };
    let mut captured = facts(&source, ids, false);
    captured.push(SyntaxFact::new(
        70,
        Some(ids.function),
        SyntaxFactKind::Occurrence,
        span_for(&source, "beta"),
        3,
        label("rust.function.definition"),
    ));

    let output = analyze(
        &snapshot,
        &source,
        &default_limits,
        captured.clone(),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("ambiguous definition is omitted without inventing an entity");
    assert_eq!(output.document().entities.len(), 1);
    assert!(output.document().skipped_regions.iter().any(|region| {
        region.detail == "declaration-name-unavailable"
            && region.domain == rootlight_ir::FactDomain::Entities
    }));
    let entity_coverage = output
        .report()
        .coverage()
        .domains()
        .iter()
        .find(|domain| domain.domain() == rootlight_ir::FactDomain::Entities)
        .expect("entity coverage is reported");
    assert_eq!(entity_coverage.status(), CoverageStatus::Bounded);
    assert_eq!(entity_coverage.skipped(), 1);

    let mut constrained_ir = IrLimits::default();
    constrained_ir.max_skipped_regions = 0;
    let error = analyze(
        &snapshot,
        &source,
        &limits(constrained_ir),
        captured,
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect_err("tight skipped-region quota rejects before lowering growth");
    assert!(matches!(
        error,
        AdapterError::Sink(SinkError::StreamLimit {
            resource: rootlight_adapter_sdk::ResourceKind::Records,
            ..
        })
    ));
}

#[test]
fn missing_java_field_definition_is_reserved_in_preflight_quotas() {
    const JAVA: &str = "class Example { int first, second; }\n";
    let (_temporary, snapshot, source) =
        source_fixture_for(JAVA, "src/Example.java", b"java-multi-field-fixture");
    let facts = vec![
        SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source.span(),
            0,
            label("java.source.root"),
        ),
        SyntaxFact::new(
            2,
            Some(1),
            SyntaxFactKind::Declaration,
            span_in(JAVA, &source, "int first, second", 0),
            1,
            label("java.field.declaration"),
        ),
    ];
    let language = LanguageId::new("java").expect("Java language is valid");

    let mut skipped_limit = IrLimits::default();
    skipped_limit.max_skipped_regions = 0;
    let error = analyze_custom(
        &snapshot,
        &source,
        language.clone(),
        &limits(skipped_limit),
        facts.clone(),
    )
    .expect_err("missing definition is admitted against skipped quota");
    assert!(matches!(
        error,
        AdapterError::Sink(SinkError::StreamLimit {
            resource: rootlight_adapter_sdk::ResourceKind::Records,
            observed: 1,
            limit: 0,
        })
    ));

    let mut total_limit = IrLimits::default();
    // Without the reserved name-unavailable record this conservative upper
    // bound would be 13 and would incorrectly pass the preflight.
    total_limit.max_total_records = 13;
    let error = analyze_custom(&snapshot, &source, language, &limits(total_limit), facts)
        .expect_err("missing definition is included in total-record preflight");
    assert!(matches!(
        error,
        AdapterError::Sink(SinkError::StreamLimit {
            resource: rootlight_adapter_sdk::ResourceKind::Records,
            observed: 14,
            limit: 13,
        })
    ));
}

#[test]
fn rust_impl_without_reviewed_owner_capture_becomes_an_explicit_gap() {
    const RUST: &str = "impl A { fn same(&self) {} }\n";
    let (_temporary, snapshot, source) =
        source_fixture_for(RUST, "src/lib.rs", b"missing-impl-owner-fixture");
    let facts = vec![
        SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source.span(),
            0,
            label("rust.file.root"),
        ),
        SyntaxFact::new(
            2,
            Some(1),
            SyntaxFactKind::Scope,
            span_in(RUST, &source, RUST.trim_end(), 0),
            1,
            label("rust.impl.scope"),
        ),
        SyntaxFact::new(
            3,
            Some(2),
            SyntaxFactKind::Declaration,
            span_in(RUST, &source, "fn same(&self) {}", 0),
            2,
            label("rust.function.declaration"),
        ),
        SyntaxFact::new(
            4,
            Some(3),
            SyntaxFactKind::Occurrence,
            span_in(RUST, &source, "same", 0),
            3,
            label("rust.function.definition"),
        ),
    ];
    let output = analyze_custom(
        &snapshot,
        &source,
        language(),
        &limits(IrLimits::default()),
        facts,
    )
    .expect("unsupported impl identity commits an explicit partial document");

    assert!(
        output
            .document()
            .entities
            .iter()
            .all(|entity| entity.canonical_name != "same")
    );
    assert!(output.document().skipped_regions.iter().any(|region| {
        region.domain == rootlight_ir::FactDomain::Entities
            && region.detail == "stable-scope-identity-unavailable"
    }));
}

#[test]
fn lowering_emits_only_evidence_backed_conservative_relations() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let output = analyze(
        &snapshot,
        &source,
        &limits,
        facts(
            &source,
            LocalIds {
                root: 1,
                module: 2,
                comment: 3,
                function: 4,
                call: 5,
                import: 6,
            },
            false,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("lowering commits");
    let document = output.document();

    assert_eq!(document.entities.len(), 2);
    let function = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "alpha")
        .expect("definition capture names the function");
    assert_eq!(
        function
            .evidence
            .source
            .as_ref()
            .expect("entity has direct definition evidence")
            .span(),
        span_for_nth(&source, "alpha", 1)
    );
    assert!(document.occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            && occurrence.source.span() == span_for_nth(&source, "alpha", 1)
    }));
    assert!(document.occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(function.id)
            && matches!(
                occurrence.target,
                rootlight_ir::OccurrenceTarget::Unresolved { .. }
            )
    }));
    assert!(document.occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::ImportUse
            && matches!(
                occurrence.target,
                rootlight_ir::OccurrenceTarget::Unresolved { .. }
            )
    }));
    assert!(!document.relations.is_empty());
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate != RelationPredicate::Calls)
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate != RelationPredicate::Imports)
    );
    assert!(document.skipped_regions.iter().any(|region| {
        region.domain == rootlight_ir::FactDomain::Relations
            && region.detail == "unresolved-import-target"
    }));

    assert_every_record_has_evidence(document);
    let lexical_kinds: Vec<_> = document
        .extensions
        .iter()
        .map(|extension| {
            decode_lexical_evidence_envelope(extension)
                .expect("first-party lexical envelope validates")
                .kind()
        })
        .collect();
    assert!(lexical_kinds.contains(&LexicalEvidenceKind::Signature));
    assert!(lexical_kinds.contains(&LexicalEvidenceKind::DocumentationSummary));
}

#[test]
fn included_ranges_and_parser_recovery_remain_explicit_coverage_gaps() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let module = span_for(&source, SOURCE.trim_end());
    let call = span_for(&source, "beta()");
    let diagnostic = AdapterDiagnostic::new(
        DiagnosticCode::new("syntax-error-recovery").expect("diagnostic code is valid"),
        DiagnosticSeverity::Warning,
        Some(source_for_span(&source, call)),
        CoverageStatus::Unknown,
    );
    let covered = usize::try_from(module.end_byte() - module.start_byte())
        .expect("fixture range length fits");
    let provider = provider(
        facts(
            &source,
            LocalIds {
                root: 1,
                module: 2,
                comment: 3,
                function: 4,
                call: 5,
                import: 6,
            },
            false,
        ),
        vec![diagnostic],
        CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Unknown,
            SOURCE.len(),
            covered,
            1,
            Vec::new(),
        )
        .expect("partial coverage is valid"),
    );
    let analyzer = analyzer(provider, &source);
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).expect("snapshot binds"),
        language(),
        EncodingId::utf8(),
        vec![IncludedRange::new(module, language())],
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"build-context")),
        &limits,
    )
    .expect("included-range analysis request is valid")
    .with_generated_status(false);
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(),
    )
    .expect("partial lowering commits");

    assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
    assert!(output.report().coverage().skipped_regions() >= 2);
    assert_eq!(output.document().diagnostics.len(), 1);
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|region| { region.detail == "outside-included-ranges" })
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|region| { region.detail == "syntax-error-recovery" })
    );
    let file_coverage = output
        .report()
        .coverage()
        .domains()
        .iter()
        .find(|domain| domain.domain() == rootlight_ir::FactDomain::Files)
        .expect("file coverage is reported");
    let diagnostic_coverage = output
        .report()
        .coverage()
        .domains()
        .iter()
        .find(|domain| domain.domain() == rootlight_ir::FactDomain::Diagnostics)
        .expect("diagnostic coverage is reported");
    assert_eq!(file_coverage.status(), CoverageStatus::Unknown);
    assert_eq!(file_coverage.skipped(), 1);
    assert_eq!(diagnostic_coverage.status(), CoverageStatus::Unknown);
    assert_eq!(diagnostic_coverage.skipped(), 1);
}

#[test]
fn canonical_document_round_trips_through_bounded_decoder() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let output = analyze(
        &snapshot,
        &source,
        &limits,
        facts(
            &source,
            LocalIds {
                root: 1,
                module: 2,
                comment: 3,
                function: 4,
                call: 5,
                import: 6,
            },
            false,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("lowering commits");
    let encoded = serde_json::to_vec(output.document()).expect("canonical document encodes");
    let decoded = decode_ir_document(&encoded, limits.ir(), &ExtensionSupport::default())
        .expect("bounded decoder accepts lowering output");

    assert_eq!(
        decoded,
        IrDocument::NormalizedV1_1(output.document().clone())
    );
}

#[test]
fn cancellation_and_ir_limits_abort_without_committed_output() {
    let (_temporary, snapshot, source) = source_fixture();
    let analysis_limits = limits(IrLimits::default());
    let provider = provider(
        facts(
            &source,
            LocalIds {
                root: 1,
                module: 2,
                comment: 3,
                function: 4,
                call: 5,
                import: 6,
            },
            false,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .with_cancellation_after_batches(0, CancellationReason::ClientRequest);
    let analyzer = analyzer(provider, &source);
    let request = request(&snapshot, &source, &analysis_limits);
    assert!(matches!(
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &deadline(),
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));

    let mut constrained_ir = IrLimits::default();
    constrained_ir.max_entities = 1;
    let constrained_limits = limits(constrained_ir);
    let error = analyze(
        &snapshot,
        &source,
        &constrained_limits,
        facts(
            &source,
            LocalIds {
                root: 11,
                module: 12,
                comment: 13,
                function: 14,
                call: 15,
                import: 16,
            },
            false,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect_err("entity quota aborts the transaction");
    assert!(matches!(
        error,
        AdapterError::Sink(SinkError::StreamLimit { .. })
    ));
}

#[test]
fn malformed_fact_errors_and_analyzer_debug_are_source_free() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let function = span_for(&source, "pub fn alpha() { beta(); }");
    let malformed = SyntaxFact::new(
        7,
        Some(999),
        SyntaxFactKind::Declaration,
        function,
        1,
        SyntaxKindLabel::new("function_item").expect("syntax label is valid"),
    );
    let provider = provider(vec![malformed], Vec::new(), complete_coverage(SOURCE.len()));
    let analyzer = analyzer(provider, &source);
    let request = request(&snapshot, &source, &limits);
    let error = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(),
    )
    .expect_err("missing parent is rejected");

    assert!(matches!(error, AdapterError::ProviderFailed { .. }));
    assert!(!format!("{error:?}").contains("docs for alpha"));
    assert!(!format!("{analyzer:?}").contains("docs for alpha"));
}

#[test]
fn non_utf8_analysis_identity_is_rejected_before_parser_execution() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let provider = provider(Vec::new(), Vec::new(), complete_coverage(SOURCE.len()));
    let analyzer = analyzer(provider, &source);
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).expect("snapshot binds"),
        language(),
        EncodingId::new("utf-16").expect("test encoding label is valid"),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"build-context")),
        &limits,
    )
    .expect("analysis request carries an explicit encoding")
    .with_generated_status(false);

    assert_eq!(
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &deadline(),
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::UnsupportedEncoding
        ))
    );
}

#[test]
fn analyzer_requires_source_classification_and_records_exact_frontend() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let provider = provider(Vec::new(), Vec::new(), complete_coverage(SOURCE.len()));
    let analyzer = analyzer(provider, &source);
    let unclassified = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source).expect("snapshot binds"),
        language(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"build-context")),
        &limits,
    )
    .expect("compatibility request remains constructible");

    assert_eq!(
        analyzer.descriptor().memory_enforcement(),
        MemoryEnforcement::Unavailable
    );
    assert_eq!(
        execute_analysis(
            &analyzer,
            &unclassified,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &deadline(),
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::GeneratedStatusRequired
        ))
    );

    let classified = unclassified.with_generated_status(true);
    let output = execute_analysis(
        &analyzer,
        &classified,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(),
    )
    .expect("classified request lowers");
    assert!(output.document().files[0].generated);
    assert_eq!(
        output.document().provenance[0].frontend_version.as_deref(),
        Some("tree-sitter-rust-0.24.2")
    );
}

#[test]
fn annotated_java_uses_definition_and_signature_captures_only() {
    const JAVA: &str =
        "@interface Marker { String value(); } class Example { @Override() void foo() {} }\n";
    let (_temporary, snapshot, source) =
        source_fixture_for(JAVA, "src/Example.java", b"java-lowering-fixture");
    let limits = limits(IrLimits::default());
    let facts = vec![
        SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source.span(),
            0,
            label("java.source.root"),
        ),
        SyntaxFact::new(
            2,
            Some(1),
            SyntaxFactKind::Declaration,
            span_in(JAVA, &source, "@interface Marker { String value(); }", 0),
            1,
            label("java.annotation.declaration"),
        ),
        SyntaxFact::new(
            3,
            Some(2),
            SyntaxFactKind::Occurrence,
            span_in(JAVA, &source, "Marker", 0),
            2,
            label("java.annotation.definition"),
        ),
        SyntaxFact::new(
            4,
            Some(2),
            SyntaxFactKind::Declaration,
            span_in(JAVA, &source, "String value()", 0),
            2,
            label("java.annotation_element.declaration"),
        ),
        SyntaxFact::new(
            5,
            Some(4),
            SyntaxFactKind::Occurrence,
            span_in(JAVA, &source, "value", 0),
            3,
            label("java.annotation_element.definition"),
        ),
        SyntaxFact::new(
            6,
            Some(4),
            SyntaxFactKind::Signature,
            span_in(JAVA, &source, "()", 0),
            3,
            label("java.annotation_element.signature"),
        ),
        SyntaxFact::new(
            7,
            Some(1),
            SyntaxFactKind::Declaration,
            span_in(
                JAVA,
                &source,
                "class Example { @Override() void foo() {} }",
                0,
            ),
            1,
            label("java.class.declaration"),
        ),
        SyntaxFact::new(
            8,
            Some(7),
            SyntaxFactKind::Occurrence,
            span_in(JAVA, &source, "Example", 0),
            2,
            label("java.class.definition"),
        ),
        SyntaxFact::new(
            9,
            Some(7),
            SyntaxFactKind::Declaration,
            span_in(JAVA, &source, "@Override() void foo() {}", 0),
            2,
            label("java.method.declaration"),
        ),
        SyntaxFact::new(
            10,
            Some(9),
            SyntaxFactKind::Occurrence,
            span_in(JAVA, &source, "foo", 0),
            3,
            label("java.method.definition"),
        ),
        SyntaxFact::new(
            11,
            Some(9),
            SyntaxFactKind::Signature,
            span_in(JAVA, &source, "()", 2),
            3,
            label("java.method.signature"),
        ),
        SyntaxFact::new(
            12,
            Some(9),
            SyntaxFactKind::Occurrence,
            span_in(JAVA, &source, "Override", 0),
            3,
            label("java.annotation.reference"),
        ),
    ];

    let output = analyze_custom(
        &snapshot,
        &source,
        LanguageId::new("java").expect("Java language is valid"),
        &limits,
        facts,
    )
    .expect("annotated Java lowers");
    let names: Vec<_> = output
        .document()
        .entities
        .iter()
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert!(names.contains(&"Example"));
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"Marker"));
    assert!(names.contains(&"value"));
    assert!(!names.contains(&"Override"));
    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            && occurrence.source.span() == span_in(JAVA, &source, "foo", 0)
    }));
}

#[test]
fn overloads_with_distinct_signature_captures_keep_distinct_symbols() {
    const OVERLOADS: &str = "fn alpha(x: i32) {}\nfn alpha(x: u64) {}\n";
    let (_temporary, snapshot, source) =
        source_fixture_for(OVERLOADS, "src/lib.rs", b"overload-lowering-fixture");
    let limits = limits(IrLimits::default());
    let mut facts = vec![SyntaxFact::new(
        1,
        None,
        SyntaxFactKind::Root,
        source.span(),
        0,
        label("rust.source.root"),
    )];
    for (offset, signature) in ["(x: i32)", "(x: u64)"].into_iter().enumerate() {
        let local = u64::try_from(offset)
            .expect("fixture offset fits")
            .checked_mul(10)
            .and_then(|value| value.checked_add(2))
            .expect("fixture local ID fits");
        let declaration_text = if offset == 0 {
            "fn alpha(x: i32) {}"
        } else {
            "fn alpha(x: u64) {}"
        };
        facts.extend([
            SyntaxFact::new(
                local,
                Some(1),
                SyntaxFactKind::Declaration,
                span_in(OVERLOADS, &source, declaration_text, 0),
                1,
                label("rust.function.declaration"),
            ),
            SyntaxFact::new(
                local + 1,
                Some(local),
                SyntaxFactKind::Occurrence,
                span_in(OVERLOADS, &source, "alpha", offset),
                2,
                label("rust.function.definition"),
            ),
            SyntaxFact::new(
                local + 2,
                Some(local),
                SyntaxFactKind::Signature,
                span_in(OVERLOADS, &source, signature, 0),
                2,
                label("rust.function.signature"),
            ),
        ]);
    }

    let output = analyze_custom(&snapshot, &source, language(), &limits, facts)
        .expect("overloads lower from exact captures");
    assert_eq!(output.document().entities.len(), 2);
    assert_eq!(output.document().entities[0].canonical_name, "alpha");
    assert_eq!(output.document().entities[1].canonical_name, "alpha");
    assert_ne!(
        output.document().entities[0].id,
        output.document().entities[1].id
    );
}

#[test]
fn python_and_javascript_file_modules_use_repository_paths() {
    for (language_name, module_label, path, source_text, repository_seed) in [
        (
            "python",
            "python.file.module",
            "src/main.py",
            "print('x')\n",
            b"python-file-module".as_slice(),
        ),
        (
            "javascript",
            "javascript.file.module",
            "src/main.js",
            "console.log('x');\n",
            b"javascript-file-module".as_slice(),
        ),
    ] {
        let (_temporary, snapshot, source) = source_fixture_for(source_text, path, repository_seed);
        let limits = limits(IrLimits::default());
        let facts = vec![
            SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Root,
                source.span(),
                0,
                label("source.root"),
            ),
            SyntaxFact::new(
                2,
                Some(1),
                SyntaxFactKind::Module,
                source.span(),
                1,
                label(module_label),
            ),
        ];
        let output = analyze_custom(
            &snapshot,
            &source,
            LanguageId::new(language_name).expect("fixture language is valid"),
            &limits,
            facts,
        )
        .expect("file module lowers from explicit path rule");

        assert_eq!(output.document().entities.len(), 1);
        assert_eq!(output.document().entities[0].canonical_name, path);
        assert_eq!(
            output.document().entities[0].kind,
            rootlight_ir::EntityKind::Module
        );
        assert_eq!(
            output.document().entities[0].flags,
            vec![EntityFlag::Synthetic]
        );
        assert!(
            output.document().occurrences.is_empty(),
            "an implicit file module has no declaration spelling"
        );
    }
}

#[test]
fn oversized_lexical_captures_truncate_or_become_explicit_gaps() {
    let comment_text = format!("// {}", "a".repeat(700));
    let source_text = format!("{comment_text}\n");
    let (_temporary, snapshot, source) =
        source_fixture_for(&source_text, "src/lib.rs", b"large-comment-fixture");
    let default_limits = limits(IrLimits::default());
    let comment_output = analyze_custom(
        &snapshot,
        &source,
        language(),
        &default_limits,
        vec![
            SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Root,
                source.span(),
                0,
                label("rust.source.root"),
            ),
            SyntaxFact::new(
                2,
                Some(1),
                SyntaxFactKind::Comment,
                span_in(&source_text, &source, &comment_text, 0),
                1,
                label("rust.comment"),
            ),
        ],
    )
    .expect("large comment uses bounded lexical truncation");
    let evidence = decode_lexical_evidence_envelope(&comment_output.document().extensions[0])
        .expect("truncated comment evidence validates");
    assert!(evidence.is_truncated());

    let parameters = format!("({})", "parameter_name: i32,".repeat(20));
    let declaration_text = format!("fn alpha{parameters} {{}}\n");
    let (_temporary, snapshot, source) =
        source_fixture_for(&declaration_text, "src/lib.rs", b"large-signature-fixture");
    let mut ir = IrLimits::default();
    ir.max_string_bytes = 100;
    let constrained = limits(ir);
    let signature_output = analyze_custom(
        &snapshot,
        &source,
        language(),
        &constrained,
        vec![
            SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Root,
                source.span(),
                0,
                label("rust.source.root"),
            ),
            SyntaxFact::new(
                2,
                Some(1),
                SyntaxFactKind::Declaration,
                span_in(&declaration_text, &source, declaration_text.trim_end(), 0),
                1,
                label("rust.function.declaration"),
            ),
            SyntaxFact::new(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                span_in(&declaration_text, &source, "alpha", 0),
                2,
                label("rust.function.definition"),
            ),
            SyntaxFact::new(
                4,
                Some(2),
                SyntaxFactKind::Signature,
                span_in(&declaration_text, &source, &parameters, 0),
                2,
                label("rust.function.signature"),
            ),
        ],
    )
    .expect("oversized signature is omitted without failing analysis");
    assert!(signature_output.document().extensions.is_empty());
    assert!(
        signature_output
            .document()
            .skipped_regions
            .iter()
            .any(|region| region.detail == "signature-capture-unavailable")
    );
}

#[test]
fn symbol_ids_survive_body_crlf_changes_and_incremental_style_reparse() {
    const BEFORE: &str = "fn alpha() {\n    beta();\n}\n";
    const AFTER: &str = "fn alpha() {\r\n        beta();   \r\n}\r\n";
    let current = std::env::current_dir().expect("current directory is available");
    let temporary = tempdir_in(current).expect("local temporary directory is available");
    fs::create_dir(temporary.path().join("src")).expect("fixture directory is created");
    let absolute = temporary.path().join("src").join("lib.rs");
    fs::write(&absolute, BEFORE).expect("initial fixture is written");
    let repository_id = derive_repository(b"incremental-lowering-fixture").id();
    let repository =
        RepositoryRoot::open(repository_id, temporary.path()).expect("temporary root opens");
    let path = RelativePath::parse(Path::new("src/lib.rs")).expect("fixture path is valid");
    let before_snapshot = repository
        .snapshot(&path, 4096)
        .expect("initial snapshot is stable");
    let before_source = source_ref_for_snapshot(repository_id, &before_snapshot);

    fs::write(&absolute, AFTER).expect("edited fixture is written");
    let after_snapshot = repository
        .snapshot(&path, 4096)
        .expect("edited snapshot is stable");
    let after_source = source_ref_for_snapshot(repository_id, &after_snapshot);
    assert_eq!(before_source.span().file(), after_source.span().file());

    let limits = limits(IrLimits::default());
    let before = analyze_custom(
        &before_snapshot,
        &before_source,
        language(),
        &limits,
        single_function_facts(BEFORE, &before_source, 10, false),
    )
    .expect("initial syntax facts lower");
    let after = analyze_custom(
        &after_snapshot,
        &after_source,
        language(),
        &limits,
        single_function_facts(AFTER, &after_source, 900, true),
    )
    .expect("incremental-style facts lower after CRLF body edit");

    assert_eq!(before.document().entities.len(), 1);
    assert_eq!(after.document().entities.len(), 1);
    assert_eq!(
        before.document().entities[0].id,
        after.document().entities[0].id
    );
}

fn analyze(
    snapshot: &SourceSnapshot,
    source: &SourceRef,
    limits: &AnalysisLimits,
    facts: Vec<SyntaxFact>,
    diagnostics: Vec<AdapterDiagnostic>,
    coverage: CoverageReport,
) -> Result<rootlight_adapter_sdk::AnalysisOutput, AdapterError> {
    let analyzer = analyzer(provider(facts, diagnostics, coverage), source);
    execute_analysis(
        &analyzer,
        &request(snapshot, source, limits),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(),
    )
}

fn analyzer(provider: MockParseProvider, source: &SourceRef) -> TreeSitterAnalyzer {
    TreeSitterAnalyzer::new(
        Arc::new(provider),
        ProducerIdentity::new(
            "rootlight-treesitter-lowering",
            "1.0",
            content_hash(b"lowering-configuration"),
        )
        .expect("producer identity is valid"),
        language(),
        "tree-sitter-rust-0.24.2",
        content_hash(source.content_hash().as_bytes()),
    )
    .expect("analyzer configuration is valid")
}

fn provider(
    facts: Vec<SyntaxFact>,
    diagnostics: Vec<AdapterDiagnostic>,
    coverage: CoverageReport,
) -> MockParseProvider {
    MockParseProvider::new(capabilities(), facts, diagnostics, coverage)
}

fn capabilities() -> ParseCapabilities {
    ParseCapabilities::new(
        vec![language()],
        vec![EncodingId::utf8()],
        4096,
        4096,
        64,
        8,
        true,
        true,
        true,
        4,
        MemoryEnforcement::AccountedInProcess,
    )
    .expect("parser capabilities are valid")
}

fn facts(source: &SourceRef, ids: LocalIds, reverse: bool) -> Vec<SyntaxFact> {
    let root = SyntaxFact::new(
        ids.root,
        None,
        SyntaxFactKind::Root,
        source.span(),
        0,
        label("source_file"),
    );
    let module_span = span_for(source, SOURCE.trim_end());
    let module = SyntaxFact::new(
        ids.module,
        Some(ids.root),
        SyntaxFactKind::Module,
        module_span,
        1,
        label("rust.module.declaration"),
    );
    let module_definition = SyntaxFact::new(
        ids.module.wrapping_add(1_000_000),
        Some(ids.module),
        SyntaxFactKind::Occurrence,
        span_for(source, "api"),
        2,
        label("rust.module.definition"),
    );
    let comment = SyntaxFact::new(
        ids.comment,
        Some(ids.module),
        SyntaxFactKind::Comment,
        span_for(source, "/// docs for alpha"),
        2,
        label("doc_comment"),
    );
    let function = SyntaxFact::new(
        ids.function,
        Some(ids.module),
        SyntaxFactKind::Declaration,
        span_for(source, "pub fn alpha() { beta(); }"),
        2,
        label("rust.function.declaration"),
    );
    let function_definition = SyntaxFact::new(
        ids.function.wrapping_add(2_000_000),
        Some(ids.function),
        SyntaxFactKind::Occurrence,
        span_for_nth(source, "alpha", 1),
        3,
        label("rust.function.definition"),
    );
    let function_signature = SyntaxFact::new(
        ids.function.wrapping_add(3_000_000),
        Some(ids.function),
        SyntaxFactKind::Signature,
        span_for(source, "()"),
        3,
        label("rust.function.signature"),
    );
    let scope = SyntaxFact::new(
        ids.function.wrapping_add(4_000_000),
        Some(ids.function),
        SyntaxFactKind::Scope,
        span_for(source, "{ beta(); }"),
        3,
        label("rust.block.scope"),
    );
    let call = SyntaxFact::new(
        ids.call,
        Some(ids.function.wrapping_add(4_000_000)),
        SyntaxFactKind::Occurrence,
        span_for(source, "beta()"),
        4,
        label("rust.call.reference"),
    );
    let import = SyntaxFact::new(
        ids.import,
        Some(ids.module),
        SyntaxFactKind::Import,
        span_for(source, "use crate::dep;"),
        2,
        label("use_declaration"),
    );
    let mut facts = vec![
        root,
        module,
        module_definition,
        comment,
        function,
        function_definition,
        function_signature,
        scope,
        call,
        import,
    ];
    if reverse {
        facts.reverse();
    }
    facts
}

fn assert_every_record_has_evidence(document: &rootlight_ir::NormalizedIrDocument) {
    assert!(
        document
            .files
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .entities
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .occurrences
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .relations
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .coverage_records
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .diagnostics
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .extensions
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(document.provenance.iter().all(|record| {
        !record.input_sources.is_empty()
            || !record.evidence_sources.is_empty()
            || !record.derivation_parents.is_empty()
    }));
}

fn has_evidence(evidence: &FactEvidence) -> bool {
    evidence.source.is_some() || !evidence.derivation.is_empty()
}

fn source_fixture() -> (TempDir, SourceSnapshot, SourceRef) {
    source_fixture_for(SOURCE, "src/lib.rs", b"treesitter-lowering-fixture")
}

fn source_fixture_for(
    source_text: &str,
    relative_path: &str,
    repository_seed: &[u8],
) -> (TempDir, SourceSnapshot, SourceRef) {
    let current = std::env::current_dir().expect("current directory is available");
    let temporary = tempdir_in(current).expect("local temporary directory is available");
    let relative = Path::new(relative_path);
    if let Some(parent) = relative.parent() {
        fs::create_dir_all(temporary.path().join(parent))
            .expect("fixture source directory is created");
    }
    fs::write(temporary.path().join(relative), source_text).expect("fixture source is written");
    let repository_id = derive_repository(repository_seed).id();
    let repository =
        RepositoryRoot::open(repository_id, temporary.path()).expect("temporary root opens");
    let path = RelativePath::parse(relative).expect("fixture relative path is valid");
    let snapshot = repository
        .snapshot(&path, 4096)
        .expect("fixture snapshot is stable");
    let byte_length = u64::try_from(snapshot.content().len()).expect("fixture length fits");
    let span = SourceSpan::new(snapshot.file(), 0, byte_length).expect("full-file span is ordered");
    let source = SourceRef::new(
        repository_id,
        GenerationId::from_bytes([7; 20]),
        span,
        snapshot.content_hash(),
        None,
    );
    (temporary, snapshot, source)
}

fn analyze_custom(
    snapshot: &SourceSnapshot,
    source: &SourceRef,
    language: LanguageId,
    limits: &AnalysisLimits,
    facts: Vec<SyntaxFact>,
) -> Result<rootlight_adapter_sdk::AnalysisOutput, AdapterError> {
    let capabilities = ParseCapabilities::new(
        vec![language.clone()],
        vec![EncodingId::utf8()],
        4096,
        4096,
        64,
        8,
        true,
        true,
        true,
        4,
        MemoryEnforcement::AccountedInProcess,
    )
    .expect("custom parser capabilities are valid");
    let provider = MockParseProvider::new(
        capabilities,
        facts,
        Vec::new(),
        complete_coverage(snapshot.content().len()),
    );
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(provider),
        ProducerIdentity::new(
            "rootlight-treesitter-lowering",
            "1.0",
            content_hash(b"lowering-configuration"),
        )
        .expect("producer identity is valid"),
        language.clone(),
        "tree-sitter-fixture-1.0",
        content_hash(b"fixture-binary"),
    )
    .expect("custom analyzer is valid");
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(snapshot, source).expect("custom snapshot binds"),
        language,
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"build-context")),
        limits,
    )
    .expect("custom analysis request is valid")
    .with_generated_status(false);
    execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &deadline(),
    )
}

fn source_ref_for_snapshot(
    repository: rootlight_ids::RepositoryId,
    snapshot: &SourceSnapshot,
) -> SourceRef {
    let byte_length = u64::try_from(snapshot.content().len()).expect("fixture length fits");
    SourceRef::new(
        repository,
        GenerationId::from_bytes([7; 20]),
        SourceSpan::new(snapshot.file(), 0, byte_length).expect("full-file span is ordered"),
        snapshot.content_hash(),
        None,
    )
}

fn single_function_facts(
    source_text: &str,
    source: &SourceRef,
    first_local_id: u64,
    reverse: bool,
) -> Vec<SyntaxFact> {
    let root = first_local_id;
    let declaration = first_local_id.wrapping_add(1);
    let mut facts = vec![
        SyntaxFact::new(
            root,
            None,
            SyntaxFactKind::Root,
            source.span(),
            0,
            label("rust.source.root"),
        ),
        SyntaxFact::new(
            declaration,
            Some(root),
            SyntaxFactKind::Declaration,
            span_in(source_text, source, source_text.trim_end(), 0),
            1,
            label("rust.function.declaration"),
        ),
        SyntaxFact::new(
            first_local_id.wrapping_add(2),
            Some(declaration),
            SyntaxFactKind::Occurrence,
            span_in(source_text, source, "alpha", 0),
            2,
            label("rust.function.definition"),
        ),
        SyntaxFact::new(
            first_local_id.wrapping_add(3),
            Some(declaration),
            SyntaxFactKind::Signature,
            span_in(source_text, source, "()", 0),
            2,
            label("rust.function.signature"),
        ),
    ];
    if reverse {
        facts.reverse();
    }
    facts
}

fn request<'a>(
    snapshot: &'a SourceSnapshot,
    source: &SourceRef,
    limits: &'a AnalysisLimits,
) -> AnalysisRequest<'a> {
    AnalysisRequest::new(
        GenerationBoundSnapshot::new(snapshot, source).expect("snapshot binds"),
        language(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"build-context")),
        limits,
    )
    .expect("analysis request is valid")
    .with_generated_status(false)
}

fn limits(ir: IrLimits) -> AnalysisLimits {
    let batch =
        BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).expect("batch thresholds are valid");
    let stream = StreamLimits::new(
        128,
        1024,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        batch,
    )
    .expect("stream limits are valid");
    AnalysisLimits::new(
        4096,
        4096,
        64,
        8,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        ir,
    )
    .expect("analysis limits are valid")
}

fn complete_coverage(source_bytes: usize) -> CoverageReport {
    CoverageReport::new(
        AnalysisTier::TierD,
        CoverageStatus::Complete,
        source_bytes,
        source_bytes,
        0,
        Vec::new(),
    )
    .expect("complete coverage is valid")
}

fn span_for(source: &SourceRef, needle: &str) -> SourceSpan {
    let start = SOURCE.find(needle).expect("fixture needle exists");
    let end = start
        .checked_add(needle.len())
        .expect("fixture span does not overflow");
    SourceSpan::new(
        source.span().file(),
        u64::try_from(start).expect("fixture start fits"),
        u64::try_from(end).expect("fixture end fits"),
    )
    .expect("fixture span is ordered")
}

fn span_for_nth(source: &SourceRef, needle: &str, index: usize) -> SourceSpan {
    span_in(SOURCE, source, needle, index)
}

fn span_in(source_text: &str, source: &SourceRef, needle: &str, index: usize) -> SourceSpan {
    let start = source_text
        .match_indices(needle)
        .nth(index)
        .map(|(start, _)| start)
        .expect("fixture occurrence exists");
    let end = start
        .checked_add(needle.len())
        .expect("fixture span does not overflow");
    SourceSpan::new(
        source.span().file(),
        u64::try_from(start).expect("fixture start fits"),
        u64::try_from(end).expect("fixture end fits"),
    )
    .expect("fixture span is ordered")
}

fn source_for_span(source: &SourceRef, span: SourceSpan) -> SourceRef {
    SourceRef::new(
        source.repository(),
        source.generation(),
        span,
        source.content_hash(),
        None,
    )
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixture syntax label is valid")
}

fn language() -> LanguageId {
    LanguageId::new("rust").expect("fixture language is valid")
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline derives"),
    )
}
