//! Bare default-export references retain exact written Class identity.
//! Complete originals, physical encodings and replayed facts use the same proof.

use super::*;
use rootlight_adapter_sdk::{
    SyntaxFact, SyntaxFactKind, SyntaxKindLabel, testkit::MockParseProvider,
};
use rootlight_ir::NormalizedIrDocument;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/javascript_method_var_reads/curve.babel.js");
const LABEL: &str = "javascript.default_class_export.reference";

fn case() -> LanguageCase {
    *CASES.iter().find(|case| case.name == "javascript").unwrap()
}

fn output(source: &[u8]) -> AnalysisOutput {
    let native = Arc::new(provider());
    let case = case();
    let fixture = Fixture::new(case, source);
    let budget = limits();
    analyze(
        &analyzer(&native, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    )
}

fn bound(
    document: &NormalizedIrDocument,
    bytes: &[u8],
    name: &str,
    definition: (u64, u64),
    reference: (u64, u64),
) {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && (
                    site.source.span().start_byte(),
                    site.source.span().end_byte(),
                ) == definition
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("written Class resolves")
    };
    let class = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(class.kind, EntityKind::Class);
    assert_eq!(class.language, "javascript");
    assert_eq!(class.canonical_name, name);
    assert_eq!(definitions[0].source.content_hash(), content_hash(bytes));
    let context = class.evidence.source.as_ref().unwrap();
    assert_eq!(definitions[0].evidence.source.as_ref(), Some(context));
    assert!(
        context.span().start_byte() <= definition.0 && definition.1 <= context.span().end_byte()
    );
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && (
                    site.source.span().start_byte(),
                    site.source.span().end_byte(),
                ) == reference
        })
        .collect();
    assert_eq!(sites.len(), 1);
    let site = sites[0];
    assert_eq!(site.syntax_kind, LABEL);
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
    assert_eq!(site.source.content_hash(), content_hash(bytes));
    assert_eq!(site.repository, document.repository);
    assert_eq!(site.generation, document.generation);
    assert_eq!(site.provenance, class.provenance);
    assert_eq!(site.evidence.source.as_ref(), Some(&site.source));
    let module = match class.container {
        Some(rootlight_ir::ContainerRef::Entity(module)) => module,
        _ => panic!("Class belongs to module"),
    };
    assert_eq!(site.enclosing, Some(module));
    assert_eq!(
        site.id,
        rootlight_ir::derive_occurrence_record_id(site).unwrap()
    );
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
    assert!(document.occurrences.iter().all(|other| (
        other.source.span().start_byte(),
        other.source.span().end_byte()
    ) != reference
        || !matches!(other.role, OccurrenceRole::Read | OccurrenceRole::CallSite)));
}

fn unbound(document: &NormalizedIrDocument, span: (u64, u64)) {
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::Reference
                || (
                    site.source.span().start_byte(),
                    site.source.span().end_byte()
                ) != span
                || !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn complete_original_curve_export_reference_binds_written_class_name() {
    assert_eq!(ORIGINAL.len(), 1480);
    assert_eq!(&ORIGINAL[37..42], b"Curve");
    assert_eq!(&ORIGINAL[1473..1478], b"Curve");
    let result = output(ORIGINAL);
    bound(result.document(), ORIGINAL, "Curve", (37, 42), (1473, 1478));
    validate_ir_document(
        result.document(),
        limits().ir(),
        &ExtensionSupport::default(),
    )
    .unwrap();
}

#[test]
fn renamed_classes_and_nested_same_spelling_keep_direct_module_identity() {
    for source in [
        "class Written {} export default Written;",
        "import Other from 'other'; class Written { method(){ class Written{} } } export default Written;",
    ] {
        let first = u64::try_from(source.find("Written").unwrap()).unwrap();
        let last = u64::try_from(source.rfind("Written").unwrap()).unwrap();
        bound(
            output(source.as_bytes()).document(),
            source.as_bytes(),
            "Written",
            (first, first + 7),
            (last, last + 7),
        );
    }
}

#[test]
fn competing_root_bindings_and_nonbare_or_nested_exports_stay_unproved() {
    for (source, spelling) in [
        ("class Name {} const other=0; export default Name;", "Name"),
        (
            "class Name {} class \\u004eame {} export default Name;",
            "Name",
        ),
        (
            "class Name {} import \\u004eame from 'other'; export default Name;",
            "Name",
        ),
        ("class Name extends make() {} export default Name;", "Name"),
        ("class Name<T> {} export default Name;", "Name"),
        ("class Name implements Face {} export default Name;", "Name"),
        ("class eval {} export default eval;", "eval"),
        ("class Name {} var Name; export default Name;", "Name"),
        (
            "class Name {} function Name(){} export default Name;",
            "Name",
        ),
        ("class Name {} { var Name; } export default Name;", "Name"),
        (
            "class Name {} import Name from 'other'; export default Name;",
            "Name",
        ),
        (
            "class Name {} import {other as Name} from 'other'; export default Name;",
            "Name",
        ),
        ("class Name {} class Name {} export default Name;", "Name"),
        (
            "function outer(){ class Name{} } export default Name;",
            "Name",
        ),
        ("class Name {} export default (Name);", "Name"),
        ("class Name {} export default Name.member;", "Name"),
        ("class Name {} export default new Name;", "Name"),
        ("class Name {} export default call(Name);", "Name"),
        ("class Name {} export {Name as default};", "Name"),
        (
            "class Name {} export {Name as default} from 'other';",
            "Name",
        ),
        ("class Name {} enum Name { X } export default Name;", "Name"),
        (
            "class Name {} namespace Name {} export default Name;",
            "Name",
        ),
    ] {
        let start = u64::try_from(source.rfind(spelling).unwrap()).unwrap();
        let end = start + u64::try_from(spelling.len()).unwrap();
        assert_eq!(
            &source.as_bytes()[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
            spelling.as_bytes()
        );
        unbound(output(source.as_bytes()).document(), (start, end));
    }
}

#[test]
fn actual_native_class_name_root_and_export_witness_tampering_refuses_binding() {
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
    let replay = |facts| {
        let mock = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(mock),
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
    };
    bound(
        replay(parsed.facts().to_vec()).unwrap().document(),
        ORIGINAL,
        "Curve",
        (37, 42),
        (1473, 1478),
    );
    for (kind, start, end) in [
        (SyntaxFactKind::Root, 0, 1480),
        (SyntaxFactKind::Module, 0, 1480),
        (SyntaxFactKind::Declaration, 31, 1456),
        (SyntaxFactKind::Occurrence, 37, 42),
        (SyntaxFactKind::Signature, 1473, 1478),
        (SyntaxFactKind::Occurrence, 1473, 1478),
    ] {
        let witnesses: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == kind
                    && fact.span().start_byte() == start
                    && fact.span().end_byte() == end
            })
            .collect();
        assert_eq!(witnesses.len(), 1);
        let witness = witnesses[0];
        let missing: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| fact.local_id() != witness.local_id())
            .cloned()
            .collect();
        let has_children = parsed
            .facts()
            .iter()
            .any(|fact| fact.parent() == Some(witness.local_id()));
        if matches!(
            kind,
            SyntaxFactKind::Root | SyntaxFactKind::Module | SyntaxFactKind::Declaration
        ) {
            assert!(has_children);
            assert!(
                missing
                    .iter()
                    .any(|fact| fact.parent() == Some(witness.local_id()))
            );
            assert!(
                missing
                    .iter()
                    .all(|fact| fact.local_id() != witness.local_id())
            );
            assert!(
                matches!(replay(missing), Err(AdapterError::ProviderFailed { code })
                    if code.as_str() == "treesitter-lowering-parent")
            );
        } else {
            assert!(!has_children);
            unbound(replay(missing).unwrap().document(), (1473, 1478));
        }
        let mut duplicate = parsed.facts().to_vec();
        duplicate.push(SyntaxFact::new(
            duplicate.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
            witness.parent(),
            witness.kind(),
            witness.span(),
            witness.depth(),
            witness.syntax_kind().clone(),
        ));
        unbound(replay(duplicate).unwrap().document(), (1473, 1478));
    }
    let definition = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.kind() == SyntaxFactKind::Occurrence
                && fact.span().start_byte() == 37
                && fact.span().end_byte() == 42
        })
        .unwrap();
    let mut changed = parsed.facts().to_vec();
    for fact in &mut changed {
        if fact.local_id() == definition.local_id() {
            *fact = SyntaxFact::new(
                fact.local_id(),
                fact.parent(),
                fact.kind(),
                SourceSpan::new(fact.span().file(), 1473, 1478).unwrap(),
                fact.depth(),
                fact.syntax_kind().clone(),
            );
        }
    }
    let parent = parsed
        .facts()
        .iter()
        .find(|fact| Some(fact.local_id()) == definition.parent())
        .unwrap();
    assert_eq!(parent.kind(), SyntaxFactKind::Declaration);
    assert_eq!(
        (parent.span().start_byte(), parent.span().end_byte()),
        (31, 1456)
    );
    assert!(parent.span().end_byte() < 1473);
    assert!(
        matches!(replay(changed), Err(AdapterError::ProviderFailed { code })
            if code.as_str() == "treesitter-lowering-parent")
    );
}

#[test]
fn forced_certificate_labels_cannot_adopt_an_unsupported_current_module() {
    for source in [
        "class Name {} const other=0; export default Name;",
        "function outer(){ class Name{} } export default Name;",
    ] {
        let native = provider();
        let case = case();
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
            CoverageStatus::Complete
        );
        let start = u64::try_from(source.rfind("Name").unwrap()).unwrap();
        let mut facts = parsed.facts().to_vec();
        let mut changed = 0;
        for fact in &mut facts {
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.span().start_byte() == start
                && fact.span().end_byte() == start + 4
                && fact.syntax_kind().as_str().ends_with(".reference")
            {
                *fact = SyntaxFact::new(
                    fact.local_id(),
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    SyntaxKindLabel::new(LABEL).unwrap(),
                );
                changed += 1;
            }
        }
        assert_eq!(changed, 1);
        let mock = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(mock),
            producer_identity(),
            language(case),
            case.frontend,
            content_hash(BINARY_SEED),
        )
        .unwrap();
        let result = analyze(&frontend, &input, &ExtensionSupport::default());
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == LABEL && site.source.span().start_byte() == start)
        );
        unbound(result.document(), (start, start + 4));
    }
}

#[test]
fn exact_required_native_and_artifact_replay_keep_export_reference_and_cancel() {
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
    bound(first.document(), ORIGINAL, "Curve", (37, 42), (1473, 1478));
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
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    bound(replay.document(), ORIGINAL, "Curve", (37, 42), (1473, 1478));
    let small = limits_with_syntax_records(required - 1);
    assert!(
        matches!(frontend.prepare_artifact(&request(&fixture.snapshot,&fixture.source,case,&small),&deadline()),Err(AdapterError::Sink(SinkError::StreamLimit { resource:ResourceKind::RequiredSyntaxFacts,observed,limit })) if observed==required && limit==required-1)
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
        matches!(frontend.analyze_from_artifact(&request(&changed.snapshot,&changed.source,case,&exact),&artifact,ExtensionSupport::default(),MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,&deadline()),Err(AdapterError::ProviderFailed { code }) if code.as_str()=="treesitter-structural-artifact-mismatch")
    );
}

#[test]
fn physical_bom_crlf_and_utf16_exports_keep_current_original_coordinates() {
    let text = std::str::from_utf8(ORIGINAL).unwrap();
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let logical = text.replace('\n', "\r\n");
        let bytes = if encoding == "utf-8" {
            [b"\xef\xbb\xbf".as_slice(), logical.as_bytes()].concat()
        } else {
            let mut bytes = if encoding == "utf-16le" {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in logical.encode_utf16() {
                bytes.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            bytes
        };
        let native = Arc::new(provider());
        let case = case();
        let fixture = Fixture::new(case, &bytes);
        let budget = limits();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
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
        let physical = |offset: usize| {
            u64::try_from(if encoding == "utf-8" {
                3 + offset
                    + text.as_bytes()[..offset]
                        .iter()
                        .filter(|b| **b == b'\n')
                        .count()
            } else {
                2 + 2
                    * (offset
                        + text.as_bytes()[..offset]
                            .iter()
                            .filter(|b| **b == b'\n')
                            .count())
            })
            .unwrap()
        };
        bound(
            result.document(),
            &bytes,
            "Curve",
            (physical(37), physical(42)),
            (physical(1473), physical(1478)),
        );
    }
}

#[test]
fn included_source_ranges_cannot_borrow_whole_module_export_authority() {
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
    unbound(
        analyze(
            &analyzer(&native, case),
            &input,
            &ExtensionSupport::default(),
        )
        .document(),
        (1473, 1478),
    );
}

#[test]
fn old_native_facts_cannot_certify_a_same_width_current_export_name() {
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
    assert!(
        parsed
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == LABEL)
    );
    let mut changed = ORIGINAL.to_vec();
    changed[1473..1478].copy_from_slice(b"Other");
    let current = fixture.rewrite(&changed);
    let mock = MockParseProvider::new(
        native.capabilities().clone(),
        parsed.facts().to_vec(),
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let frontend = TreeSitterAnalyzer::new(
        Arc::new(mock),
        producer_identity(),
        language(case),
        case.frontend,
        content_hash(BINARY_SEED),
    )
    .unwrap();
    let result = analyze(
        &frontend,
        &request(&current.snapshot, &current.source, case, &budget),
        &ExtensionSupport::default(),
    );
    unbound(result.document(), (1473, 1478));
}
