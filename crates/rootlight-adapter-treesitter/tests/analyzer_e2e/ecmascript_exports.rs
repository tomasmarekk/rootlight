//! Native export metadata and callable signature independence.
//! Export target fields must survive required-budget replay without becoming
//! callable headers or changing authored declaration identities.

use super::*;

#[test]
fn namespace_export_fields_retain_type_intent_and_exact_public_name_references() {
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        for public in ["Space", "'naïve-space'"] {
            let modifier = if case.name == "typescript" {
                "type "
            } else {
                ""
            };
            let source =
                format!("// café\r\nexport {modifier}* as {public} from './provider';\r\n");
            let provider = Arc::new(provider());
            let analyzer = analyzer(&provider, case);
            let fixture = Fixture::new(case, source.as_bytes());
            let budget = limits();
            let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
            let parsed = rootlight_adapter_sdk::execute_parse(
                provider.as_ref(),
                &initial.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
            for (suffix, text) in [
                ("export_namespace_name.signature", public.to_owned()),
                ("export_name.reference", public.to_owned()),
                (
                    if modifier.is_empty() {
                        "export_namespace_statement.signature"
                    } else {
                        "export_type_namespace_statement.signature"
                    },
                    format!("export {modifier}* as {public} from './provider';"),
                ),
            ] {
                assert!(
                    parsed
                        .facts()
                        .iter()
                        .any(|fact| fact.syntax_kind().as_str().ends_with(suffix)
                            && source[usize::try_from(fact.span().start_byte()).unwrap()
                                ..usize::try_from(fact.span().end_byte()).unwrap()]
                                == text),
                    "{source}: {suffix}"
                );
            }
            let (_, artifact) = analyzer
                .analyze_and_capture(
                    &initial,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
            let next = fixture.next_generation();
            let next_request = request(&next.snapshot, &next.source, case, &budget);
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
        }
    }
}

#[test]
fn reexport_metadata_replays_with_exact_module_and_name_fields() {
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        for (source, expected) in [
            (
                "export {Actual as Public} from './provider';\r\n",
                vec![
                    "'./provider'",
                    "Actual",
                    "Actual as Public",
                    "Public",
                    "export {Actual as Public} from './provider';",
                ],
            ),
            (
                "export * from './provider';\r\n",
                vec!["'./provider'", "export * from './provider';"],
            ),
            (
                "export * as Actual from './provider';\r\n",
                vec![
                    "'./provider'",
                    "Actual",
                    "export * as Actual from './provider';",
                ],
            ),
        ] {
            let provider = Arc::new(provider());
            let analyzer = analyzer(&provider, case);
            let fixture = Fixture::new(case, source.as_bytes());
            let budget = limits();
            let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
            let required = provider
                .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
                .unwrap();
            let bounded = limits_with_syntax_records(required);
            let initial = request(&fixture.snapshot, &fixture.source, case, &bounded);
            let parsed = rootlight_adapter_sdk::execute_parse(
                provider.as_ref(),
                &initial.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
            let mut metadata: Vec<_> = parsed
                .facts()
                .iter()
                .filter(|fact| {
                    fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature
                        && fact.syntax_kind().as_str().contains(".export_")
                })
                .map(|fact| {
                    &source[usize::try_from(fact.span().start_byte()).unwrap()
                        ..usize::try_from(fact.span().end_byte()).unwrap()]
                })
                .collect();
            metadata.sort_unstable();
            assert_eq!(metadata, expected);
            let (_, artifact) = analyzer
                .analyze_and_capture(
                    &initial,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
            let next = fixture.next_generation();
            let next_request = request(&next.snapshot, &next.source, case, &bounded);
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
        }
    }
}

#[test]
fn named_export_metadata_survives_required_budget_and_replay() {
    let source =
        "export function Actual(value) { return value; }\r\nexport {Actual as Public};\r\n";
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
        let required = provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap();
        let bounded = limits_with_syntax_records(required);
        let initial = request(&fixture.snapshot, &fixture.source, case, &bounded);
        let parsed = rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
            &initial.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let mut metadata: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature
                    && fact.syntax_kind().as_str().contains(".export_")
            })
            .map(|fact| {
                &source[usize::try_from(fact.span().start_byte()).unwrap()
                    ..usize::try_from(fact.span().end_byte()).unwrap()]
            })
            .collect();
        metadata.sort_unstable();
        assert_eq!(
            metadata,
            [
                "Actual",
                "Actual as Public",
                "Public",
                "function Actual(value) { return value; }"
            ]
        );
        let (_, artifact) = analyzer
            .analyze_and_capture(
                &initial,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let next = fixture.next_generation();
        let next_request = request(&next.snapshot, &next.source, case, &bounded);
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
    }
}

#[test]
fn default_export_metadata_replays_without_polluting_callable_signatures() {
    let source = "export default function Actual(value) { return value; }\r\n";
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
        let required = provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap();
        let bounded = limits_with_syntax_records(required);
        let initial = request(&fixture.snapshot, &fixture.source, case, &bounded);
        let parsed = rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
            &initial.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let export: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str().contains(".default_export_"))
            .collect();
        assert_eq!(export.len(), 1);
        assert_eq!(
            &source[usize::try_from(export[0].span().start_byte()).unwrap()
                ..usize::try_from(export[0].span().end_byte()).unwrap()],
            "function Actual(value) { return value; }"
        );
        let (first, artifact) = analyzer
            .analyze_and_capture(
                &initial,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let actual = symbol_id_named(first.document(), "Actual");
        let signatures: Vec<_> = first
            .document()
            .extensions
            .iter()
            .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|extension| {
                let evidence = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
                (evidence.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && evidence.subject() == rootlight_ir::FactRef::Entity(actual))
                .then_some(evidence)
            })
            .collect();
        assert_eq!(signatures.len(), 1);
        assert!(!signatures[0].text().contains("return"));
        assert!(signatures[0].text().contains("value"));
        let next = fixture.next_generation();
        let next_request = request(&next.snapshot, &next.source, case, &bounded);
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
        assert_eq!(actual, symbol_id_named(replay.document(), "Actual"));
    }
}
