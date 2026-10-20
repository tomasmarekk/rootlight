//! Type inference requires complete standalone evidence of every schema owner.
//! These real provider/analyzer cases preserve safe definitions and physical
//! evidence while withholding document-wide targets from partial source frames.

use super::*;
use rootlight_adapter_sdk::{ParseOutput, execute_parse};

fn frame(source: &str, spans: &[(u64, u64)]) -> (ParseOutput, AnalysisOutput) {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(CAPNP, source.as_bytes());
    let limits = limits();
    let ranges = spans
        .iter()
        .map(|&(start, end)| {
            IncludedRange::new(
                SourceSpan::new(fixture.snapshot.file(), start, end).unwrap(),
                LanguageId::new("capnp").unwrap(),
            )
        })
        .collect();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        LanguageId::new("capnp").unwrap(),
        EncodingId::utf8(),
        ranges,
        AnalysisTier::TierD,
        build_context(),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let parsed = execute_parse(
        provider.as_ref(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let result = analyze(
        &analyzer(&provider, CAPNP),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(
        result.document().files[0].content_hash,
        content_hash(source.as_bytes())
    );
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(source.len()).unwrap()
    );
    (parsed, result)
}

fn unresolved_item_use(result: &AnalysisOutput, source: &str) {
    let uses = result
        .document()
        .occurrences
        .iter()
        .filter(|row| row.role == OccurrenceRole::TypeUse && text(source, &row.source) == "Item")
        .collect::<Vec<_>>();
    assert_eq!(uses.len(), 1);
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(
        uses[0].source.content_hash(),
        content_hash(source.as_bytes())
    );
    let start = source.find("value @0 :Item").unwrap() + "value @0 :".len();
    assert_eq!(
        uses[0].source.span().start_byte(),
        u64::try_from(start).unwrap()
    );
    assert_eq!(
        uses[0].source.span().end_byte(),
        u64::try_from(start + "Item".len()).unwrap()
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|row| row.predicate == RelationPredicate::UsesType)
    );
    exact_definition(result, source, "Host", EntityKind::Struct, "struct Host");
    exact_definition(result, source, "value", EntityKind::Field, "value @0");
}

#[test]
fn capnp_erroneous_duplicate_cannot_leave_a_false_unique_type_target() {
    let source = "struct Item {}\nstruct Item { broken @0 : ; }\nstruct Host { value @0 :Item; }\n";
    let (parsed, result) = frame(source, &[]);
    assert_eq!(parsed.report().coverage().status(), CoverageStatus::Unknown);
    assert!(!parsed.diagnostics().is_empty());
    exact_definition(
        &result,
        source,
        "Item",
        EntityKind::Struct,
        "struct Item {}",
    );
    unresolved_item_use(&result, source);
    let withheld = source.find("struct Item { broken").unwrap();
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.detail == "capnp-declaration-name-owner-or-grammar-unavailable"
                    && gap.source.span().start_byte()
                        <= u64::try_from(withheld + "struct ".len()).unwrap()
                    && gap.source.span().end_byte()
                        >= u64::try_from(withheld + "struct Item".len()).unwrap()
            )
    );
}

#[test]
fn capnp_native_complete_parse_still_withholds_targets_when_a_declaration_is_unadmitted() {
    let source = "struct Item {}\nstruct Invalid.Name {}\nstruct Host { value @0 :Item; }\n";
    let (parsed, result) = frame(source, &[]);
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        parsed.report().coverage().covered_source_bytes(),
        source.len()
    );
    assert_eq!(parsed.report().coverage().skipped_regions(), 0);
    assert!(
        parsed
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "capnp.declaration_unavailable.signature")
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.detail == "capnp-declaration-name-owner-or-grammar-unavailable"
                    && text(source, &gap.source) == "struct Invalid.Name {}"
            )
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|row| row.canonical_name == "Invalid.Name")
    );
    exact_definition(
        &result,
        source,
        "Item",
        EntityKind::Struct,
        "struct Item {}",
    );
    unresolved_item_use(&result, source);
}

#[test]
fn capnp_included_prefix_hiding_an_owner_preserves_definitions_without_unique_targets() {
    let prefix = "struct Item {}\nstruct Host { value @0 :Item; }\n";
    let source = format!("{prefix}struct Item {{}}\n");
    let end = u64::try_from(prefix.len()).unwrap();
    let (parsed, result) = frame(&source, &[(0, end)]);
    assert_eq!(parsed.report().coverage().status(), CoverageStatus::Bounded);
    assert_eq!(
        parsed.report().coverage().covered_source_bytes(),
        prefix.len()
    );
    assert_eq!(
        parsed.report().coverage().total_source_bytes(),
        source.len()
    );
    unresolved_item_use(&result, &source);
    exact_definition(
        &result,
        &source,
        "Item",
        EntityKind::Struct,
        "struct Item {}",
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "outside-included-ranges"
                && gap.source.span().start_byte() == end
                && gap.source.span().end_byte() == u64::try_from(source.len()).unwrap())
    );
}

#[test]
fn capnp_full_span_included_frame_does_not_bypass_the_standalone_request_fence() {
    let source = "struct Item {}\nstruct Host { value @0 :Item; }\n";
    let (parsed, result) = frame(source, &[(0, u64::try_from(source.len()).unwrap())]);
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        parsed.report().coverage().covered_source_bytes(),
        source.len()
    );
    unresolved_item_use(&result, source);
    exact_definition(
        &result,
        source,
        "Item",
        EntityKind::Struct,
        "struct Item {}",
    );
}

#[test]
fn capnp_encoded_frame_keeps_physical_definitions_without_utf8_only_target_inference() {
    let source = "struct Item {} struct Host { value @0 :Item; }\n";
    let mut raw = vec![0xff, 0xfe];
    for unit in source.encode_utf16() {
        raw.extend_from_slice(&unit.to_le_bytes());
    }
    let provider = Arc::new(provider());
    let fixture = Fixture::new(CAPNP, &raw);
    let limits = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        LanguageId::new("capnp").unwrap(),
        EncodingId::new("utf-16le").unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let parsed = execute_parse(
        provider.as_ref(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let result = analyze(
        &analyzer(&provider, CAPNP),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files[0].content_hash, content_hash(&raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    let item = entity(&result, "Item", EntityKind::Struct);
    let definition = result
        .document()
        .occurrences
        .iter()
        .find(|row| {
            row.role == OccurrenceRole::Definition
                && row.target == OccurrenceTarget::Resolved { symbol: item.id }
        })
        .unwrap();
    assert_eq!(
        definition.source.span().start_byte(),
        2 + 2 * u64::try_from("struct ".len()).unwrap()
    );
    assert_eq!(
        definition.source.span().end_byte(),
        2 + 2 * u64::try_from("struct Item".len()).unwrap()
    );
    let uses = result
        .document()
        .occurrences
        .iter()
        .filter(|row| row.role == OccurrenceRole::TypeUse)
        .collect::<Vec<_>>();
    assert_eq!(uses.len(), 1);
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    let projected_start = source.find(":Item").unwrap() + 1;
    let physical_start =
        2 + 2 * u64::try_from(source[..projected_start].encode_utf16().count()).unwrap();
    assert_eq!(uses[0].source.span().start_byte(), physical_start);
    assert_eq!(uses[0].source.span().end_byte(), physical_start + 8);
    assert_eq!(uses[0].source.content_hash(), content_hash(&raw));
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|row| row.predicate == RelationPredicate::UsesType)
    );
}
