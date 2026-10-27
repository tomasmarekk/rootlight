//! Qualify current physical source and materialized ownership independently.
//! Baseline facts come from the actual analyzer before one authority is altered.

use super::super::{
    Lowering, NativeSourceView, TreeSitterAnalyzer, exclude_ambiguous_entities, materialize_entity,
};
use super::*;
use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId, GenerationBoundSnapshot,
    LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis, execute_parse,
};
use rootlight_ids::{GenerationId, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ExtensionSupport, IrLimits, ProducerIdentity,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use std::{fs, path::Path, sync::Arc};

const ORIGINAL: &[u8] = include_bytes!("../../../tests/fixtures/liquid/blog-sidebar.liquid");

fn with_native(
    raw: &[u8],
    operation: impl FnOnce(
        &Lowering<'_, '_>,
        &HashMap<u64, MaterializedEntity>,
        &SyntaxFact,
        &OccurrenceRecord,
        SymbolId,
        u64,
    ),
) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("written.liquid"), raw).unwrap();
    let repository = derive_repository(b"liquid-native-consumer-qualification").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("written.liquid")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([7; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(
        crate::TreeSitterProvider::new(
            crate::RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                16,
                64,
                1,
                16 * 1024 * 1024,
                crate::ParserSettings::new(256).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "liquid-native-consumer-qualification",
            "1.0",
            content_hash(b"liquid-consumer-config"),
        )
        .unwrap(),
        LanguageId::new("liquid").unwrap(),
        "liquid-native-consumer-qualification",
        content_hash(b"liquid-consumer-binary"),
    )
    .unwrap();
    let stream = || {
        StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap()
    };
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream(),
        stream(),
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("liquid").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"liquid-consumer-build")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let cancel = Cancellation::new();
    let parsed = execute_parse(
        provider.as_ref(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancel,
    )
    .unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        rootlight_ir::CoverageStatus::Complete
    );
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancel,
    )
    .unwrap();
    let read = output
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "liquid.loop_value.reference"
                && site.source.span().start_byte() == 270
                && site.source.span().end_byte() == 277
        })
        .expect("the actual original must supply its native certified reference before tampering");
    let OccurrenceTarget::Resolved { symbol } = read.target else {
        panic!("the baseline actual original must bind before consumer tampering");
    };
    let view = NativeSourceView::new(
        raw,
        "utf-8",
        limits.max_source_bytes(),
        limits.max_reported_memory_bytes(),
        &cancel,
    )
    .unwrap();
    let lowering = Lowering::new(&analyzer, &request, &parsed, &view, &cancel).unwrap();
    let mut strings = 0;
    let plan = lowering.entity_drafts(&cancel, &mut strings).unwrap();
    let provenance = lowering.provenance().unwrap().id;
    let mut entities = HashMap::new();
    for draft in &plan.drafts {
        let entity = materialize_entity(
            draft,
            &source,
            &lowering.coordinates,
            draft
                .authored_display_span
                .map(|span| lowering.text_for_span(span))
                .transpose()
                .unwrap(),
            request.build_context(),
            provenance,
            analyzer.descriptor.tier(),
            limits.ir().max_string_bytes,
            &entities,
        )
        .unwrap();
        entities.insert(draft.local_id, entity);
    }
    let _excluded = exclude_ambiguous_entities(&mut entities);
    let (&local, variable) = entities
        .iter()
        .find(|(_, entity)| entity.record.id == symbol)
        .unwrap();
    assert_eq!(variable.record.kind, EntityKind::Variable);
    let definition = variable.definition_local_id.unwrap();
    assert!(
        lowering
            .coordinates
            .facts()
            .iter()
            .any(|fact| fact.local_id() == definition
                && fact.span().start_byte() == 191
                && fact.span().end_byte() == 198)
    );
    let fact = lowering
        .coordinates
        .facts()
        .iter()
        .find(|fact| {
            required(fact) && fact.span().start_byte() == 270 && fact.span().end_byte() == 277
        })
        .unwrap();
    let mut unresolved = read.clone();
    unresolved.target = OccurrenceTarget::Unresolved {
        text_hash: read.syntactic_text_hash,
    };
    operation(&lowering, &entities, fact, &unresolved, symbol, local);
}

fn join(
    lowering: &Lowering<'_, '_>,
    facts: &[SyntaxFact],
    entities: &HashMap<u64, MaterializedEntity>,
    source: &SourceRef,
    cancel: &Cancellation,
) -> Result<Targets, AdapterError> {
    Targets::new(
        facts,
        entities,
        CurrentSource {
            reference: source,
            logical: lowering.source_view.as_bytes(),
            original: lowering.request.source().bytes(),
            encoding: lowering.request.encoding().as_str(),
            complete: true,
            maximum: 100_000,
        },
        |span| lowering.text_for_span(span),
        |span| lowering.coordinates.physical(span),
        cancel,
    )
}

#[test]
fn current_native_definition_and_read_endpoints_join_the_actual_variable() {
    with_native(
        ORIGINAL,
        |lowering, entities, fact, occurrence, expected, _| {
            let cancel = Cancellation::new();
            let joined = join(
                lowering,
                lowering.coordinates.facts(),
                entities,
                lowering.full_source,
                &cancel,
            )
            .unwrap();
            assert_eq!(
                joined
                    .resolve(
                        fact,
                        lowering.coordinates.physical(fact.span()).unwrap(),
                        occurrence,
                        &cancel
                    )
                    .unwrap(),
                Some(expected)
            );
        },
    );
}

#[test]
fn duplicate_fact_identity_or_duplicate_native_endpoint_cannot_select_a_target() {
    with_native(ORIGINAL, |lowering, entities, fact, occurrence, _, _| {
        let mut facts = lowering.coordinates.facts().to_vec();
        facts.push(fact.clone());
        let cancel = Cancellation::new();
        let joined = join(lowering, &facts, entities, lowering.full_source, &cancel).unwrap();
        assert_eq!(
            joined
                .resolve(
                    fact,
                    lowering.coordinates.physical(fact.span()).unwrap(),
                    occurrence,
                    &cancel
                )
                .unwrap(),
            None
        );
    });
}

#[test]
fn missing_actual_definition_or_foreign_materialized_name_never_falls_back_to_spelling() {
    with_native(
        ORIGINAL,
        |lowering, entities, fact, occurrence, _, local| {
            let cancel = Cancellation::new();
            let definition = entities[&local].definition_local_id.unwrap();
            let facts: Vec<_> = lowering
                .coordinates
                .facts()
                .iter()
                .filter(|candidate| candidate.local_id() != definition)
                .cloned()
                .collect();
            let joined = join(lowering, &facts, entities, lowering.full_source, &cancel).unwrap();
            assert_eq!(
                joined
                    .resolve(
                        fact,
                        lowering.coordinates.physical(fact.span()).unwrap(),
                        occurrence,
                        &cancel
                    )
                    .unwrap(),
                None
            );
            let mut changed = entities.clone();
            changed.get_mut(&local).unwrap().record.canonical_name = "foreign".to_owned();
            let joined = join(
                lowering,
                lowering.coordinates.facts(),
                &changed,
                lowering.full_source,
                &cancel,
            )
            .unwrap();
            assert_eq!(
                joined
                    .resolve(
                        fact,
                        lowering.coordinates.physical(fact.span()).unwrap(),
                        occurrence,
                        &cancel
                    )
                    .unwrap(),
                None
            );
        },
    );
}

#[test]
fn foreign_generation_and_physical_hash_never_admit_current_name_endpoints() {
    with_native(ORIGINAL, |lowering, entities, fact, occurrence, _, _| {
        let cancel = Cancellation::new();
        for source in [
            SourceRef::new(
                lowering.full_source.repository(),
                GenerationId::from_bytes([19; 20]),
                lowering.full_source.span(),
                lowering.full_source.content_hash(),
                None,
            ),
            SourceRef::new(
                lowering.full_source.repository(),
                lowering.full_source.generation(),
                lowering.full_source.span(),
                content_hash(b"foreign bytes"),
                None,
            ),
        ] {
            let joined = join(
                lowering,
                lowering.coordinates.facts(),
                entities,
                &source,
                &cancel,
            )
            .unwrap();
            assert_eq!(
                joined
                    .resolve(
                        fact,
                        lowering.coordinates.physical(fact.span()).unwrap(),
                        occurrence,
                        &cancel
                    )
                    .unwrap(),
                None
            );
        }
    });
}

#[test]
fn complete_source_authority_and_physical_to_logical_projection_are_both_required() {
    with_native(ORIGINAL, |lowering, entities, fact, occurrence, _, _| {
        let cancel = Cancellation::new();
        let mut other = lowering.source_view.as_bytes().to_vec();
        other[0] = b' ';
        for (logical, complete, maximum) in [
            (other.as_slice(), true, 100_000),
            (lowering.source_view.as_bytes(), false, 100_000),
            (lowering.source_view.as_bytes(), true, 1),
        ] {
            let joined = Targets::new(
                lowering.coordinates.facts(),
                entities,
                CurrentSource {
                    reference: lowering.full_source,
                    logical,
                    original: lowering.request.source().bytes(),
                    encoding: lowering.request.encoding().as_str(),
                    complete,
                    maximum,
                },
                |span| lowering.text_for_span(span),
                |span| lowering.coordinates.physical(span),
                &cancel,
            )
            .unwrap();
            assert_eq!(
                joined
                    .resolve(
                        fact,
                        lowering.coordinates.physical(fact.span()).unwrap(),
                        occurrence,
                        &cancel
                    )
                    .unwrap(),
                None
            );
        }
    });
}

#[test]
fn altered_occurrence_role_source_span_or_hash_cannot_reuse_a_certificate() {
    with_native(ORIGINAL, |lowering, entities, fact, occurrence, _, _| {
        let cancel = Cancellation::new();
        let joined = join(
            lowering,
            lowering.coordinates.facts(),
            entities,
            lowering.full_source,
            &cancel,
        )
        .unwrap();
        let mut wrong_role = occurrence.clone();
        wrong_role.role = OccurrenceRole::Reference;
        let mut wrong_hash = occurrence.clone();
        wrong_hash.syntactic_text_hash = content_hash(b"foreign");
        let mut wrong_frame = occurrence.clone();
        wrong_frame.generation = GenerationId::from_bytes([23; 20]);
        for changed in [wrong_role, wrong_hash, wrong_frame] {
            assert_eq!(
                joined
                    .resolve(
                        fact,
                        lowering.coordinates.physical(fact.span()).unwrap(),
                        &changed,
                        &cancel
                    )
                    .unwrap(),
                None
            );
        }
    });
}

#[test]
fn cancelled_consumer_does_not_replace_cancellation_with_an_unresolved_result() {
    with_native(ORIGINAL, |lowering, entities, _, _, _, _| {
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            join(
                lowering,
                lowering.coordinates.facts(),
                entities,
                lowering.full_source,
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    });
}

#[test]
fn altered_parent_depth_and_enclosing_owner_cannot_reuse_a_retained_local_id() {
    with_native(ORIGINAL, |lowering, entities, fact, occurrence, _, _| {
        let cancel = Cancellation::new();
        let joined = join(
            lowering,
            lowering.coordinates.facts(),
            entities,
            lowering.full_source,
            &cancel,
        )
        .unwrap();
        let changed = SyntaxFact::new(
            fact.local_id(),
            None,
            fact.kind(),
            fact.span(),
            fact.depth(),
            fact.syntax_kind().clone(),
        );
        assert_eq!(
            joined
                .resolve(
                    &changed,
                    lowering.coordinates.physical(fact.span()).unwrap(),
                    occurrence,
                    &cancel
                )
                .unwrap(),
            None
        );
        let changed = SyntaxFact::new(
            fact.local_id(),
            fact.parent(),
            fact.kind(),
            fact.span(),
            fact.depth() + 1,
            fact.syntax_kind().clone(),
        );
        assert_eq!(
            joined
                .resolve(
                    &changed,
                    lowering.coordinates.physical(fact.span()).unwrap(),
                    occurrence,
                    &cancel
                )
                .unwrap(),
            None
        );
        let mut changed = occurrence.clone();
        changed.enclosing = None;
        assert_eq!(
            joined
                .resolve(
                    fact,
                    lowering.coordinates.physical(fact.span()).unwrap(),
                    &changed,
                    &cancel
                )
                .unwrap(),
            None
        );
    });
}
