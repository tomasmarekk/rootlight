//! Exact C formal certificates survive legitimate artifact rebinding only.
//! Existing analyzer fixtures provide the real source/cache admission path.

use super::*;

#[test]
fn original_formal_certificate_rebinds_but_stale_foreign_and_cancelled_artifacts_refuse() {
    let case = *CASES.iter().find(|case| case.name == "c").unwrap();
    let raw = include_bytes!("../fixtures/c_parameters/callbacks.c");
    let primary_provider = Arc::new(provider());
    let primary = analyzer(&primary_provider, case);
    let bounds = limits();
    let fixture = Fixture::new(case, raw);
    let original_request = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let (_, artifact) = primary
        .analyze_and_capture(
            &original_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let successor = fixture.next_generation();
    let current = request(&successor.snapshot, &successor.source, case, &bounds);
    let rebound = primary
        .analyze_from_artifact(
            &current,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let clean = analyze(&primary, &current, &ExtensionSupport::default());
    assert_eq!(rebound.document(), clean.document());
    assert_eq!(rebound.report(), clean.report());
    let formal = rebound
        .document()
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Parameter && e.canonical_name == "loop")
        .unwrap();
    let sites: Vec<_> = rebound
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 115
                && site.source.span().end_byte() == 119
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: formal.id }
    );
    assert_eq!(sites[0].generation, successor.source.generation());
    assert_eq!(sites[0].source.content_hash(), content_hash(raw));
    assert_eq!(sites[0].source.generation(), successor.source.generation());
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        primary_provider
            .required_syntax_fact_count(&current.to_parse_request(), &deadline())
            .unwrap()
    );
    let other = analyzer(&Arc::new(provider()), case);
    assert!(
        other
            .analyze_from_artifact(
                &current,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        primary.analyze_from_artifact(
            &current,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation
        ),
        Err(AdapterError::Cancelled { .. })
    ));
    let changed = fixture.rewrite(
        &String::from_utf8(raw.to_vec())
            .unwrap()
            .replace("accept_cb(loop", "other_cb(loop")
            .into_bytes(),
    );
    let changed_request = request(&changed.snapshot, &changed.source, case, &bounds);
    assert!(
        primary
            .analyze_from_artifact(
                &changed_request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
}

#[test]
fn native_c_protocol_labels_keep_the_exact_function_context() {
    let case = *CASES.iter().find(|case| case.name == "c").unwrap();
    let raw = include_bytes!("../fixtures/c_parameters/callbacks.c");
    assert_eq!(raw.len(), 146);
    let native = provider();
    let fixture = Fixture::new(case, raw);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let roots: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Root)
        .collect();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].syntax_kind().as_str(), "c.file.root");
    assert_eq!(roots[0].span().start_byte(), 0);
    assert_eq!(
        roots[0].span().end_byte(),
        u64::try_from(raw.len()).unwrap()
    );
    let owners: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| {
            fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == "c.function.declaration"
        })
        .collect();
    let contexts: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| {
            fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "c.formal_binding_context.signature"
        })
        .collect();
    assert_eq!(owners.len(), 1);
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].span(), owners[0].span());
    assert!(parsed.facts().iter().any(|fact| fact.kind()
        == rootlight_adapter_sdk::SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "c.identifier.definition"
        && fact.span().start_byte() == 31
        && fact.span().end_byte() == 42));
    assert!(!parsed.facts().iter().any(|fact| matches!(
        fact.syntax_kind().as_str(),
        "c.translation_unit.root" | "c.function_definition.declaration"
    )));
}
