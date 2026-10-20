//! Native depth refusal preserves bounded uncertainty without weakening quotas.
//! The original regression exercises the same parser and artifact path as service.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/flux/original-stack_overflow.flux");
const REASON: &str = "flux-native-depth-unavailable";

fn assert_decline(output: &AnalysisOutput, raw: &[u8]) {
    let document = output.document();
    assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].content_hash, content_hash(raw));
    assert!(document.entities.is_empty());
    assert!(document.occurrences.is_empty());
    assert!(document.relations.is_empty());
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        let gaps: Vec<_> = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.domain == domain && gap.detail == REASON)
            .collect();
        assert_eq!(gaps.len(), 1);
        let source = &gaps[0].source;
        assert_eq!(source.content_hash(), content_hash(raw));
        assert_eq!(
            source.span(),
            SourceSpan::new(document.files[0].id, 0, raw.len() as u64).unwrap()
        );
        assert!(
            document
                .provenance
                .iter()
                .any(|provenance| provenance.id == gaps[0].provenance)
        );
    }
    let diagnostics: Vec<_> = document
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == REASON)
        .collect();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].severity,
        rootlight_ir::DiagnosticSeverity::Warning
    );
    assert_eq!(diagnostics[0].coverage_effect, CoverageStatus::Unknown);
    let source = diagnostics[0].source.as_ref().unwrap();
    assert_eq!(source.content_hash(), content_hash(raw));
    assert_eq!(
        source.span(),
        SourceSpan::new(document.files[0].id, 0, raw.len() as u64).unwrap()
    );
}

#[test]
fn original_depth_stop_becomes_an_exact_unknown_artifact() {
    assert_eq!(ORIGINAL.len(), 7717);
    with_case(
        ORIGINAL,
        EncodingId::utf8(),
        IrLimits::default(),
        16_384,
        128,
        |analyzer, provider, request| {
            let cancel = Cancellation::new();
            let parsed_request = request.to_parse_request();
            let required = provider.required_syntax_fact_count(&parsed_request, &cancel)?;
            assert_eq!(required, 3);
            let parsed = rootlight_adapter_sdk::execute_parse(
                provider,
                &parsed_request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            assert_eq!(parsed.facts().len(), required);
            assert_eq!(parsed.report().coverage().status(), CoverageStatus::Unknown);
            assert_eq!(parsed.diagnostics().len(), 1);
            assert_eq!(
                parsed.diagnostics()[0].source(),
                Some(request.source().source_ref())
            );
            let artifact = analyzer.prepare_artifact(request, &cancel)?;
            assert_eq!(artifact.syntax_fact_count(), required);
            let output = execute_analysis(
                analyzer,
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            let cached = analyzer.analyze_prepared_artifact(
                request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            assert_eq!(output.document(), cached.document());
            assert_decline(&output, ORIGINAL);
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn depth_decline_utf16_keeps_whole_physical_source() {
    let text = std::str::from_utf8(ORIGINAL).unwrap();
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            raw.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let output = analyze_raw(
            &raw,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
            IrLimits::default(),
            16_384,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        assert_decline(&output, &raw);
    }
}

#[test]
fn below_native_depth_bound_keeps_bindings_and_stricter_caller_depth_still_refuses() {
    let source = "measure = (value) => consume(v: value)\n";
    let output = analyze(source, 16_384, 64, &Cancellation::new()).unwrap();
    let target = symbol(output.document(), source, "(value)", "value");
    assert_eq!(
        read(output.document(), source.rfind("value").unwrap(), "value").target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    assert!(output.report().resources().syntax_nodes() > 0);
    assert!(
        !output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == REASON)
    );
    assert!(
        matches!(analyze(source, 16_384, 3, &Cancellation::new()), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "flux-native-depth-limit")
    );
}

#[test]
fn depth_decline_does_not_mask_cancellation_or_syntax_record_quota() {
    with_case(
        ORIGINAL,
        EncodingId::utf8(),
        IrLimits::default(),
        16_384,
        128,
        |_, provider, request| {
            let cancelled = Cancellation::new();
            cancelled.cancel(CancellationReason::ClientRequest);
            assert!(matches!(
                provider.required_syntax_fact_count(&request.to_parse_request(), &cancelled),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            let original = request.limits();
            let stream = original.syntax_stream();
            let batch = stream.batch();
            let syntax = StreamLimits::new(
                stream.max_batches(),
                2,
                stream.max_output_bytes(),
                stream.max_diagnostics(),
                stream.max_diagnostic_bytes(),
                stream.max_string_bytes(),
                BatchThresholds::new(
                    2,
                    batch.max_output_bytes(),
                    batch.max_diagnostics(),
                    batch.max_diagnostic_bytes(),
                )
                .unwrap(),
            )
            .unwrap();
            let limits = AnalysisLimits::new(
                original.max_source_bytes(),
                original.max_syntax_nodes(),
                original.max_syntax_depth(),
                original.max_embedded_ranges(),
                original.max_reported_memory_bytes(),
                syntax,
                original.ir_stream().clone(),
                original.ir().clone(),
            )
            .unwrap();
            let limited = rootlight_adapter_sdk::ParseRequest::new(
                request.source().clone(),
                LanguageId::new("flux").unwrap(),
                EncodingId::utf8(),
                Vec::new(),
                &limits,
            )
            .unwrap();
            assert!(matches!(
                rootlight_adapter_sdk::execute_parse(
                    provider,
                    &limited,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new()
                ),
                Err(AdapterError::Sink(
                    rootlight_adapter_sdk::SinkError::StreamLimit {
                        resource: rootlight_adapter_sdk::ResourceKind::Records,
                        observed: 3,
                        limit: 2
                    }
                ))
            ));
            Ok(())
        },
    )
    .unwrap();
}
