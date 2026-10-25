//! Unsupported TSX declarations cannot borrow a JavaScript method-var label.
//! Forced raw facts exercise the independent current-source refusal boundary.

use super::*;
use rootlight_adapter_sdk::{
    SyntaxFact, SyntaxFactKind, SyntaxKindLabel, testkit::MockParseProvider,
};

fn observed(source: &str, force_certificate: bool) -> (AnalysisOutput, (u64, u64)) {
    let native = provider();
    let case = *CASES.iter().find(|case| case.name == "javascript").unwrap();
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete,
        "complete native evidence must reach the independent join: {source}"
    );
    let start = u64::try_from(source.rfind("return local").unwrap() + 7).unwrap();
    let span = (start, start + 5);
    let mut facts = parsed.facts().to_vec();
    let selected: Vec<_> = facts
        .iter()
        .enumerate()
        .filter(|(_, fact)| {
            fact.kind() == SyntaxFactKind::Occurrence
                && fact.span().start_byte() == span.0
                && fact.span().end_byte() == span.1
                && fact.syntax_kind().as_str().ends_with(".reference")
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(selected.len(), 1, "one actual selected Reference: {source}");
    if force_certificate {
        let index = selected[0];
        let old = facts[index].clone();
        facts[index] = SyntaxFact::new(
            old.local_id(),
            old.parent(),
            old.kind(),
            old.span(),
            old.depth(),
            SyntaxKindLabel::new("javascript.method_var.reference").unwrap(),
        );
    }
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
    let result = execute_analysis(
        &frontend,
        &input,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    (result, span)
}

#[test]
fn forced_method_var_labels_cannot_resolve_current_tsx_declaration_sources() {
    let ordinary = "class A { run(){ var local=1; return local === other; } }";
    let (positive, span) = observed(ordinary, true);
    assert_eq!(
        positive
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == span.0
                    && site.source.span().end_byte() == span.1
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            })
            .count(),
        1,
        "the same actual raw join first succeeds on ordinary source"
    );
    for (kind, source) in [
        (
            "function_signature",
            "class A { run(){ var local=1; function local(); return local === other; } }",
        ),
        (
            "abstract_class_declaration",
            "class A { run(){ var local=1; abstract class local {} return local === other; } }",
        ),
        (
            "module",
            "class A { run(){ var local=1; module local {} return local === other; } }",
        ),
        (
            "internal_module",
            "class A { run(){ var local=1; namespace local {} return local === other; } }",
        ),
        (
            "type_alias_declaration",
            "class A { run(){ var local=1; type local = number; return local === other; } }",
        ),
        (
            "enum_declaration",
            "class A { run(){ var local=1; enum local { X } return local === other; } }",
        ),
        (
            "interface_declaration",
            "class A { run(){ var local=1; interface local {} return local === other; } }",
        ),
        (
            "import_alias",
            "class A { run(){ var local=1; import local = other; return local === other; } }",
        ),
        (
            "ambient_declaration",
            "class A { run(){ var local=1; declare global {} return local === other; } }",
        ),
        (
            "enum_declaration",
            "class A { run(){ var local=1; const enum local { X } return local === other; } }",
        ),
        (
            "module",
            "class A { run(){ var local=1; module local.nested {} return local === other; } }",
        ),
        (
            "module",
            "class A { run(){ var local=1; module \"local\" {} return local === other; } }",
        ),
        (
            "import_alias",
            "class A { run(){ var local=1; import local = other.member; return local === other; } }",
        ),
    ] {
        let (result, span) = observed(source, true);
        let document = result.document();
        let references: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == span.0
                    && site.source.span().end_byte() == span.1
                    && site.syntax_kind == "javascript.method_var.reference"
            })
            .collect();
        assert_eq!(
            references.len(),
            1,
            "forced exact Reference remains visible: {kind}"
        );
        assert!(
            matches!(references[0].target, OccurrenceTarget::Unresolved { .. }),
            "fresh consumer must refuse: {kind}"
        );
        assert!(
            document
                .occurrences
                .iter()
                .all(|site| site.role != OccurrenceRole::Read
                    || site.source.span().start_byte() != span.0
                    || site.source.span().end_byte() != span.1),
            "no Read facet after refusal: {kind}"
        );
        assert!(
            document
                .relations
                .iter()
                .all(|edge| edge.predicate != RelationPredicate::Reads
                    || edge.evidence.source.as_ref().is_none_or(|source| source
                        .span()
                        .start_byte()
                        != span.0
                        || source.span().end_byte() != span.1)),
            "no entity Reads after refusal: {kind}"
        );
    }
}
