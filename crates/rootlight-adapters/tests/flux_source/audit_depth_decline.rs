//! A parsed original can exceed the separate native AST audit ancestry ceiling.
//! Only that hard ceiling permits source-only output; caller limits stay strict.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/flux/original-stack_overflow_2.flux");
const REASON: &str = "flux-native-depth-unavailable";

fn assert_parser_succeeds_before_audit(depth: usize) {
    use flux_source_parser::{
        control::{Control, Limits},
        controlled_parser::{ControlledParser, SourcePolicy},
    };
    let mut cancelled = |_| false;
    let control = Control::new(
        Limits {
            source_bytes: 262_144,
            transitions: 16_384 * 128,
            tokens: 16_384 * 8,
            positions: 16_384 * 8,
            comments: 16_384 * 8,
            parser_depth: depth,
            ast_nodes: 16_384 * 16,
        },
        &mut cancelled,
    );
    let mut parser = ControlledParser::new(
        std::str::from_utf8(ORIGINAL).unwrap(),
        control,
        SourcePolicy::LocationsOnly,
    )
    .unwrap();
    let tree = parser
        .parse_file(String::new())
        .expect("the actual original must pass the native parser before audit");
    assert_eq!(tree.body.len(), 1);
    assert!(tree.base.errors.is_empty());
}

fn assert_unknown(output: &AnalysisOutput, raw: &[u8]) {
    let document = output.document();
    assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].language, "flux");
    assert_eq!(document.files[0].content_hash, content_hash(raw));
    assert_eq!(
        document.files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
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
        assert_eq!(source.repository(), document.files[0].repository);
        assert_eq!(source.generation(), document.files[0].generation);
        assert_eq!(source.content_hash(), content_hash(raw));
        assert_eq!(
            source.span(),
            SourceSpan::new(document.files[0].id, 0, u64::try_from(raw.len()).unwrap()).unwrap()
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
    assert_eq!(diagnostics[0].coverage_effect, CoverageStatus::Unknown);
    assert_eq!(
        diagnostics[0].severity,
        rootlight_ir::DiagnosticSeverity::Warning
    );
    assert_eq!(
        diagnostics[0].source.as_ref().unwrap().span(),
        SourceSpan::new(document.files[0].id, 0, u64::try_from(raw.len()).unwrap()).unwrap()
    );
    assert_eq!(
        diagnostics[0].source.as_ref().unwrap().content_hash(),
        content_hash(raw)
    );
}

#[test]
fn original_parsed_ast_audit_depth_becomes_exact_unknown_artifact() {
    assert_eq!(ORIGINAL.len(), 1757);
    assert_parser_succeeds_before_audit(128);
    with_case(
        ORIGINAL,
        EncodingId::utf8(),
        IrLimits::default(),
        16_384,
        128,
        |analyzer, provider, request| {
            let cancel = Cancellation::new();
            let parse_request = request.to_parse_request();
            assert_eq!(
                provider.required_syntax_fact_count(&parse_request, &cancel)?,
                3
            );
            let parsed = rootlight_adapter_sdk::execute_parse(
                provider,
                &parse_request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            assert_eq!(parsed.facts().len(), 3);
            assert_eq!(parsed.report().coverage().status(), CoverageStatus::Unknown);
            assert!(parsed.report().resources().syntax_nodes() > 0);
            assert_eq!(parsed.report().resources().max_syntax_depth(), 128);
            for (fact, (kind, label, parent)) in parsed.facts().iter().zip([
                (
                    rootlight_adapter_sdk::SyntaxFactKind::Root,
                    "flux.source_file",
                    None,
                ),
                (
                    rootlight_adapter_sdk::SyntaxFactKind::Signature,
                    "flux.partial_parse.signature",
                    Some(parsed.facts()[0].local_id()),
                ),
                (
                    rootlight_adapter_sdk::SyntaxFactKind::ErrorRecovery,
                    "flux.native_depth_unavailable",
                    Some(parsed.facts()[0].local_id()),
                ),
            ]) {
                assert_eq!(fact.kind(), kind);
                assert_eq!(fact.syntax_kind().as_str(), label);
                assert_eq!(fact.parent(), parent);
                assert_eq!(fact.span(), request.source().source_ref().span());
            }
            assert_eq!(parsed.diagnostics().len(), 1);
            assert_eq!(
                parsed.diagnostics()[0].source(),
                Some(request.source().source_ref())
            );
            let artifact = analyzer.prepare_artifact(request, &cancel)?;
            assert_eq!(artifact.syntax_fact_count(), 3);
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
            assert_unknown(&output, ORIGINAL);
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn audit_depth_decline_keeps_both_utf16_physical_source_views() {
    let original = std::str::from_utf8(ORIGINAL).unwrap();
    let text = format!("// λ 😀\r\n{original}");
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
        assert_unknown(&output, &raw);
    }
}

#[test]
fn audit_depth_decline_does_not_mask_lower_depth_work_or_cancellation() {
    assert_parser_succeeds_before_audit(127);
    assert!(matches!(
        analyze_raw(ORIGINAL, EncodingId::utf8(), IrLimits::default(), 16_384, 127, &Cancellation::new()),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "flux-native-audit-depth-limit"
    ));
    let work = format!("one = (value) => \"{}\"\n", "a".repeat(100_000));
    assert!(matches!(
        analyze(&work, 32, 128, &Cancellation::new()),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "flux-native-work-limit"
    ));
    let mut ir = IrLimits::default();
    ir.max_total_records = 1;
    assert!(matches!(
        analyze_raw(ORIGINAL, EncodingId::utf8(), ir, 16_384, 128, &Cancellation::new()),
        Err(AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit {
            resource: rootlight_adapter_sdk::ResourceKind::Records, observed, limit: 1,
        })) if observed > 1
    ));
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyze_raw(
            ORIGINAL,
            EncodingId::utf8(),
            IrLimits::default(),
            16_384,
            128,
            &cancelled
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn audit_depth_decline_keeps_exact_three_fact_stream_admission() {
    with_case(
        ORIGINAL,
        EncodingId::utf8(),
        IrLimits::default(),
        16_384,
        128,
        |_, provider, request| {
            let original = request.limits();
            let stream = original.syntax_stream();
            let batch = stream.batch();
            for capacity in [3, 2] {
                let syntax = StreamLimits::new(
                    stream.max_batches(),
                    capacity,
                    stream.max_output_bytes(),
                    stream.max_diagnostics(),
                    stream.max_diagnostic_bytes(),
                    stream.max_string_bytes(),
                    BatchThresholds::new(
                        capacity,
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
                let result = rootlight_adapter_sdk::execute_parse(
                    provider,
                    &limited,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new(),
                );
                if capacity == 3 {
                    let parsed = result?;
                    assert_eq!(parsed.facts().len(), 3);
                    assert_eq!(parsed.report().coverage().status(), CoverageStatus::Unknown);
                } else {
                    assert_eq!(
                        result.unwrap_err(),
                        AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit {
                            resource: rootlight_adapter_sdk::ResourceKind::Records,
                            observed: 3,
                            limit: 2,
                        })
                    );
                }
            }
            Ok(())
        },
    )
    .unwrap();
}
