//! Source-level controls for prospective layout accounting and rollback.
//! The sealed Oracle and its independently reopened plan are the comparison
//! authority; tests do not infer restore rows from serialized normalized IR.

use std::{cell::Cell, collections::BTreeMap};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{FactId, SymbolId};
use rootlight_ir::{CoverageStatus, EntityFlag, FactRef, LineRange, OccurrenceTarget, SourceRef};
use rootlight_storage::GenerationBudget;

use super::*;
use crate::{
    OracleReader, OracleWriter,
    oracle_roundtrip_tests::{fixture_documents, snapshot},
};

fn measure(
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
) -> OracleLayoutFragment {
    OracleLayoutFragment::measure(document, 0, context, &mut |_| Ok::<_, ()>(())).unwrap()
}

fn empty(document: &NormalizedIrDocument) -> OracleLayoutFragment {
    OracleLayoutFragment::empty(document.repository, document.generation)
}

fn insert(
    census: &mut OracleLayoutCensus,
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
) {
    census
        .preview(
            empty(document),
            measure(document, context),
            0,
            context,
            &mut |_| Ok::<_, ()>(()),
        )
        .unwrap()
        .commit();
}

fn observed_plan(reader: &OracleReader, context: &GenerationContext<'_>) -> OracleReadMemoryPlan {
    let mut observed = None;
    let stopped = reader.read_with_admission(context, |plan| {
        observed = Some(plan);
        Err::<(), _>(())
    });
    assert!(matches!(
        stopped,
        Err(OracleReadAdmissionError::Admission(()))
    ));
    observed.unwrap()
}

fn flag_rows(document: &NormalizedIrDocument) -> u64 {
    document.entities.iter().fold(0_u64, |rows, entity| {
        rows.checked_add(u64::try_from(entity.flags.len()).unwrap())
            .unwrap()
    })
}

fn symbol_envelopes(document: &NormalizedIrDocument) -> u64 {
    u64::try_from(
        document
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE)
            .count(),
    )
    .unwrap()
}

fn all_flags() -> Vec<EntityFlag> {
    vec![
        EntityFlag::Generated,
        EntityFlag::External,
        EntityFlag::Test,
        EntityFlag::Exported,
        EntityFlag::Deprecated,
        EntityFlag::Synthetic,
    ]
}

fn all_layout_features() -> NormalizedIrDocument {
    let (mut document, _) = fixture_documents();
    document.extensions[0].namespace = rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE.to_owned();
    document.entities[0].flags.clear();
    let mut second = document.entities[0].clone();
    second.id = SymbolId::from_bytes([0x79; 20]);
    second.canonical_name = "other".into();
    second.display_name = "Other".into();
    second.qualified_name = "fixture.Other".into();
    second.flags = vec![EntityFlag::Test];
    document.entities.push(second);
    let mut third = document.entities[0].clone();
    third.id = SymbolId::from_bytes([0x78; 20]);
    third.canonical_name = "all_flags".into();
    third.display_name = "AllFlags".into();
    third.qualified_name = "fixture.AllFlags".into();
    third.flags = all_flags();
    document.entities.push(third);
    document.occurrences[0].target = OccurrenceTarget::Candidates {
        symbols: document.entities.iter().map(|entity| entity.id).collect(),
        total_count: 3,
        completeness: CoverageStatus::Complete,
    };
    document.entities[0]
        .evidence
        .derivation
        .push(FactRef::File(document.files[0].id));
    document.occurrences[0]
        .evidence
        .derivation
        .push(FactRef::File(document.files[0].id));
    document.relations[0]
        .evidence
        .derivation
        .push(FactRef::File(document.files[0].id));
    document.coverage_records[0]
        .evidence
        .derivation
        .push(FactRef::File(document.files[0].id));
    let whole = document.provenance[0].input_sources[0].clone();
    document.provenance[0].input_sources.push(whole.clone());
    document.provenance[0].evidence_sources.push(whole);
    // Opaque evidence is stored only in its JSON payload. Counting a separate
    // evidence child here would diverge from the actual Oracle schema.
    document.source_mappings[0]
        .evidence
        .derivation
        .push(FactRef::File(document.files[0].id));
    document.skipped_regions[0]
        .evidence
        .derivation
        .push(FactRef::File(document.files[0].id));
    document.diagnostics[0]
        .evidence
        .derivation
        .push(FactRef::File(document.files[0].id));
    document.extensions[0]
        .evidence
        .derivation
        .push(FactRef::File(document.files[0].id));
    document
}

fn categories(document: &NormalizedIrDocument) -> Vec<NormalizedIrDocument> {
    let mut result = Vec::new();
    macro_rules! category {
        ($field:ident) => {{
            let mut fragment =
                NormalizedIrDocument::empty(document.repository, document.generation);
            fragment.$field = document.$field.clone();
            result.push(fragment);
        }};
    }
    category!(files);
    category!(entities);
    category!(occurrences);
    category!(relations);
    category!(provenance);
    category!(source_mappings);
    category!(coverage_records);
    category!(skipped_regions);
    category!(diagnostics);
    category!(extensions);
    result
}

#[test]
fn incremental_census_matches_actual_writer_seal_and_reopen_for_all_layout_features() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let document = all_layout_features();
    let generation = snapshot(document);
    let document = generation.document();
    let mut census = OracleLayoutCensus::new(document.repository, document.generation);
    for fragment in categories(document) {
        insert(&mut census, &fragment, &context);
    }
    let prospective = census.read_memory_plan(&context).unwrap();
    assert_eq!(census.totals.entity_flag_rows, 7);
    assert_eq!(census.totals.symbol_envelopes, 1);
    let actual = write::measure(&generation, &context).unwrap();
    assert_eq!(prospective.stats(), actual.stats);
    assert_eq!(
        prospective.maximum_payload_bytes(),
        actual.maximum_payload_bytes
    );
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let expected_document = document.clone();
    let reader = OracleWriter::create_in(root.path())
        .unwrap()
        .seal_unverified_for_test(generation, &context)
        .unwrap();
    assert_eq!(prospective, observed_plan(&reader, &context));
    drop(reader);
    let reopened = OracleReader::open_in(root.path(), &context).unwrap();
    assert_eq!(prospective, observed_plan(&reopened, &context));
    let restored = reopened.read(&context).unwrap();
    assert_eq!(restored.document(), &expected_document);
}

#[test]
fn symbol_envelope_replacements_conserve_preview_commit_and_rollback_counts() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let (template, _) = fixture_documents();
    let mut document = NormalizedIrDocument::empty(template.repository, template.generation);
    let mut census = OracleLayoutCensus::new(document.repository, document.generation);
    insert(&mut census, &document, &context);
    for count in [3, 1, 0] {
        let before = census.read_memory_plan(&context).unwrap();
        let before_count = symbol_envelopes(&document);
        let mut replacement = NormalizedIrDocument::empty(template.repository, template.generation);
        for index in 0_u8..4 {
            let mut envelope = template.extensions[0].clone();
            envelope.id = FactId::from_bytes([index + 0x60; 20]);
            envelope.namespace = if index < count {
                rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE
            } else {
                "example.other"
            }
            .to_owned();
            replacement.extensions.push(envelope);
        }
        let expected = measure(&replacement, &context)
            .read_memory_plan(&context)
            .unwrap();
        {
            let preview = census
                .preview(
                    measure(&document, &context),
                    measure(&replacement, &context),
                    0,
                    &context,
                    &mut |_| Ok::<_, ()>(()),
                )
                .unwrap();
            assert_eq!(preview.census.totals.symbol_envelopes, u64::from(count));
            assert_eq!(preview.read_memory_plan(&context).unwrap(), expected);
        }
        assert_eq!(census.totals.symbol_envelopes, before_count);
        assert_eq!(census.read_memory_plan(&context).unwrap(), before);
        census
            .preview(
                measure(&document, &context),
                measure(&replacement, &context),
                0,
                &context,
                &mut |_| Ok::<_, ()>(()),
            )
            .unwrap()
            .commit();
        document = replacement;
        assert_eq!(census.totals.symbol_envelopes, u64::from(count));
        assert_eq!(census.read_memory_plan(&context).unwrap(), expected);
    }
    let before = census.read_memory_plan(&context).unwrap();
    let mut invalid_removal = document.clone();
    invalid_removal.extensions[0].namespace =
        rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE.to_owned();
    let error = census
        .preview(
            measure(&invalid_removal, &context),
            empty(&document),
            0,
            &context,
            &mut |_| Ok::<_, ()>(()),
        )
        .unwrap_err();
    assert!(
        matches!(error, OracleReadAdmissionError::Catalog(ref error) if error.kind() == CatalogErrorKind::InvalidGeneration)
    );
    assert_eq!(census.read_memory_plan(&context).unwrap(), before);
}

#[test]
fn flag_only_replacements_additions_and_removals_conserve_committed_and_dropped_totals() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let (template, _) = fixture_documents();
    let mut document = NormalizedIrDocument::empty(template.repository, template.generation);
    document.entities = template.entities.clone();
    document.entities[0].flags.clear();
    let mut census = OracleLayoutCensus::new(document.repository, document.generation);
    insert(&mut census, &document, &context);

    for flags in [
        Some(vec![EntityFlag::External]),
        Some(all_flags()),
        Some(Vec::new()),
        Some(all_flags()),
        None,
        Some(all_flags()),
    ] {
        let before = census.read_memory_plan(&context).unwrap();
        let before_rows = flag_rows(&document);
        let mut replacement = NormalizedIrDocument::empty(template.repository, template.generation);
        if let Some(flags) = flags {
            let mut entity = template.entities[0].clone();
            entity.flags = flags;
            replacement.entities.push(entity);
        }
        let expected_rows = flag_rows(&replacement);
        let expected = measure(&replacement, &context)
            .read_memory_plan(&context)
            .unwrap();
        {
            let preview = census
                .preview(
                    measure(&document, &context),
                    measure(&replacement, &context),
                    0,
                    &context,
                    &mut |_| Ok::<_, ()>(()),
                )
                .unwrap();
            assert_eq!(preview.census.totals.entity_flag_rows, expected_rows);
            assert_eq!(preview.read_memory_plan(&context).unwrap(), expected);
        }
        assert_eq!(census.totals.entity_flag_rows, before_rows);
        assert_eq!(census.read_memory_plan(&context).unwrap(), before);
        census
            .preview(
                measure(&document, &context),
                measure(&replacement, &context),
                0,
                &context,
                &mut |_| Ok::<_, ()>(()),
            )
            .unwrap()
            .commit();
        document = replacement;
        assert_eq!(census.totals.entity_flag_rows, expected_rows);
        assert_eq!(census.read_memory_plan(&context).unwrap(), expected);
    }

    assert_eq!(census.totals.entity_flag_rows, 6);
}

#[test]
fn source_ref_counts_keep_every_field_and_restore_after_dropped_preview() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let (mut document, _) = fixture_documents();
    let original = document.occurrences[0].source.clone();
    let alias = SourceRef::new(
        original.repository(),
        original.generation(),
        original.span(),
        original.content_hash(),
        Some(LineRange::new(2, 2).unwrap()),
    );
    document.diagnostics[0].source = Some(alias.clone());
    let mut census = OracleLayoutCensus::new(document.repository, document.generation);
    insert(&mut census, &document, &context);
    let before = census.read_memory_plan(&context).unwrap();
    assert!(census.sources.count(&original) > 1);
    assert!(census.sources.count(&alias) > 0);
    let mut removal = NormalizedIrDocument::empty(document.repository, document.generation);
    removal.diagnostics.push(document.diagnostics[0].clone());
    {
        let preview = census
            .preview(
                measure(&removal, &context),
                empty(&document),
                0,
                &context,
                &mut |_| Ok::<_, ()>(()),
            )
            .unwrap();
        assert!(
            preview
                .read_memory_plan(&context)
                .unwrap()
                .stats()
                .source_refs()
                < before.stats().source_refs()
        );
    }
    assert_eq!(before, census.read_memory_plan(&context).unwrap());
    assert!(census.sources.count(&original) > 1);
    assert!(census.sources.count(&alias) > 0);
}

#[test]
fn removing_and_replacing_maximum_payload_shrinks_the_exact_plan() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let (mut document, _) = fixture_documents();
    document.diagnostics[0].message = "opaque-payload".repeat(4096);
    let mut census = OracleLayoutCensus::new(document.repository, document.generation);
    insert(&mut census, &document, &context);
    let before = census.read_memory_plan(&context).unwrap();
    let mut removal = NormalizedIrDocument::empty(document.repository, document.generation);
    removal.diagnostics = document.diagnostics.clone();
    let mut addition = removal.clone();
    addition.diagnostics[0].id = FactId::from_bytes([0x75; 20]);
    addition.diagnostics[0].message = "small".into();
    let preview = census
        .preview(
            measure(&removal, &context),
            measure(&addition, &context),
            0,
            &context,
            &mut |_| Ok::<_, ()>(()),
        )
        .unwrap();
    let after = preview.read_memory_plan(&context).unwrap();
    assert!(after.maximum_payload_bytes() < before.maximum_payload_bytes());
    assert!(after.materialization_bytes() < before.materialization_bytes());
    preview.commit();
    document.diagnostics = addition.diagnostics;
    let actual = write::measure_document(&document, &context, None).unwrap();
    assert_eq!(
        census.read_memory_plan(&context).unwrap(),
        read::oracle_read_memory_plan(
            GENERATION_CONTRACT_VERSION,
            actual.stats,
            actual.maximum_payload_bytes,
            flag_rows(&document),
            symbol_envelopes(&document),
        )
        .unwrap()
    );
}

#[test]
fn foreign_duplicate_cancelled_overflow_and_host_refusal_do_not_change_accepted_census() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let (document, _) = fixture_documents();
    let mut census = OracleLayoutCensus::new(document.repository, document.generation);
    insert(&mut census, &document, &context);
    let before = census.read_memory_plan(&context).unwrap();
    assert!(
        census
            .preview(
                empty(&document),
                measure(&document, &context),
                0,
                &context,
                &mut |_| Ok::<_, ()>(())
            )
            .is_err()
    );
    let mut foreign = empty(&document);
    foreign.generation = GenerationId::from_bytes([0x73; 20]);
    assert!(
        census
            .preview(
                empty(&document),
                foreign,
                0,
                &context,
                &mut |_| Ok::<_, ()>(())
            )
            .is_err()
    );
    let mut overflow = empty(&document);
    overflow.totals.records[0] = u64::MAX;
    assert!(
        census
            .preview(
                empty(&document),
                overflow,
                0,
                &context,
                &mut |_| Ok::<_, ()>(())
            )
            .is_err()
    );
    let refused =
        OracleLayoutFragment::measure(&document, 0, &context, &mut |_| Err("workspace refused"));
    assert!(matches!(
        refused,
        Err(OracleReadAdmissionError::Admission("workspace refused"))
    ));
    assert_eq!(before, census.read_memory_plan(&context).unwrap());
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    let cancelled_context = GenerationContext::new(&cancelled, GenerationBudget::default());
    let called = Cell::new(false);
    let failure = census.preview(
        empty(&document),
        empty(&document),
        0,
        &cancelled_context,
        &mut |_| {
            called.set(true);
            Ok::<_, ()>(())
        },
    );
    assert!(
        matches!(failure, Err(OracleReadAdmissionError::Catalog(ref error))
        if error.kind() == CatalogErrorKind::Cancelled)
    );
    drop(failure);
    assert!(!called.get());
    assert_eq!(before, census.read_memory_plan(&context).unwrap());
}

fn assert_balanced(
    tree: &CountedTree<u64>,
    root: Option<usize>,
    low: Option<u64>,
    high: Option<u64>,
) -> i16 {
    let Some(index) = root else {
        return 0;
    };
    let node = &tree.nodes[index];
    let key = *node.key.as_ref().unwrap();
    assert!(low.is_none_or(|low| low < key));
    assert!(high.is_none_or(|high| key < high));
    assert!(node.count > 0);
    let left = assert_balanced(tree, node.left, low, Some(key));
    let right = assert_balanced(tree, node.right, Some(key), high);
    assert!((left - right).abs() <= 1);
    assert_eq!(node.height, 1 + left.max(right));
    node.height
}

#[test]
fn counted_tree_handles_ordered_insertions_removals_and_reused_slots_without_prefix_rescans() {
    let mut tree = CountedTree::<u64>::default();
    tree.reserve(8192, 0, &mut |_| Ok::<_, ()>(())).unwrap();
    let capacity = tree.nodes.capacity();
    let mut reference = BTreeMap::new();
    for key in 0..4096 {
        tree.set(&key, key + 1);
        reference.insert(key, key + 1);
    }
    assert_balanced(&tree, tree.root, None, None);
    for key in (0..4096).step_by(3) {
        tree.set(&key, 0);
        reference.remove(&key);
    }
    for key in (0..4096).rev().step_by(5) {
        tree.set(&key, 19);
        reference.insert(key, 19);
    }
    assert_balanced(&tree, tree.root, None, None);
    assert_eq!(tree.len, reference.len());
    assert_eq!(tree.last(), reference.last_key_value().map(|(key, _)| key));
    for key in 0..4096 {
        assert_eq!(tree.count(&key), reference.get(&key).copied().unwrap_or(0));
    }
    for key in (0..4096).rev() {
        tree.set(&key, 0);
    }
    assert!(tree.root.is_none());
    assert_eq!(tree.len, 0);
    for key in 4096..8192 {
        tree.set(&key, 7);
    }
    assert_eq!(tree.nodes.capacity(), capacity);
    assert_balanced(&tree, tree.root, None, None);
}

thread_local! {
    pub(super) static FAIL_NEXT_ALLOCATION: Cell<bool> = const { Cell::new(false) };
}

#[test]
fn allocation_failure_remains_catalog_storage_and_keeps_accepted_census() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let (document, _) = fixture_documents();
    let mut census = OracleLayoutCensus::new(document.repository, document.generation);
    let before = census.read_memory_plan(&context).unwrap();
    let addition = measure(&document, &context);
    FAIL_NEXT_ALLOCATION.with(|slot| slot.set(true));
    let result = census.preview(empty(&document), addition, 0, &context, &mut |_| {
        Ok::<_, ()>(())
    });
    assert!(
        matches!(result, Err(OracleReadAdmissionError::Catalog(ref error))
        if error.kind() == CatalogErrorKind::Storage)
    );
    drop(result);
    assert_eq!(before, census.read_memory_plan(&context).unwrap());
}

#[test]
fn small_opaque_fragment_counts_exact_bytes_without_physical_serializer_buffering() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let (document, _) = fixture_documents();
    let mut opaque = NormalizedIrDocument::empty(document.repository, document.generation);
    opaque.diagnostics.push(document.diagnostics[0].clone());
    write::BUFFERED_SERIALIZATIONS.with(|slot| slot.set(0));
    let mut peak = 0;
    let measured = OracleLayoutFragment::measure(&opaque, 0, &context, &mut |bytes| {
        peak = peak.max(bytes);
        Ok::<_, ()>(())
    })
    .unwrap();
    assert_eq!(write::BUFFERED_SERIALIZATIONS.with(Cell::get), 0);
    assert!(peak < u64::try_from(write::JSON_CHECKPOINT_BYTES).unwrap());
    let raw = serde_json::to_vec(&opaque.diagnostics[0]).unwrap();
    assert_eq!(measured.totals.text, u64::try_from(raw.len()).unwrap());
    assert_eq!(measured.payloads, [(u64::try_from(raw.len()).unwrap(), 1)]);
    OracleLayoutFragment::measure(&opaque, 0, &context, &mut |bytes| {
        if bytes > peak {
            Err("workspace refused")
        } else {
            Ok(())
        }
    })
    .unwrap();
    let failure = OracleLayoutFragment::measure(&opaque, 0, &context, &mut |bytes| {
        if bytes >= peak {
            Err("workspace refused")
        } else {
            Ok(())
        }
    });
    assert!(matches!(
        failure,
        Err(OracleReadAdmissionError::Admission("workspace refused"))
    ));
    assert_eq!(write::BUFFERED_SERIALIZATIONS.with(Cell::get), 0);
}

#[test]
fn replacement_that_reuses_all_capacity_still_admits_simultaneous_owned_workspace() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let (document, _) = fixture_documents();
    let mut census = OracleLayoutCensus::new(document.repository, document.generation);
    insert(&mut census, &document, &context);
    let before = census.read_memory_plan(&context).unwrap();
    let retained = census.retained_bytes().unwrap();
    let removed = measure(&document, &context);
    let added = measure(&document, &context);
    let other = 123_456;
    let required =
        other + retained + removed.retained_bytes().unwrap() + added.retained_bytes().unwrap();
    let calls = Cell::new(0);
    let rejection = census.preview(removed, added, other, &context, &mut |observed| {
        calls.set(calls.get() + 1);
        assert!(observed >= required);
        Err("replacement workspace refused")
    });
    assert!(matches!(
        rejection,
        Err(OracleReadAdmissionError::Admission(
            "replacement workspace refused"
        ))
    ));
    assert_eq!(calls.get(), 1);
    drop(rejection);
    assert_eq!(retained, census.retained_bytes().unwrap());
    assert_eq!(before, census.read_memory_plan(&context).unwrap());
}

#[test]
fn merged_owned_floors_and_target_deltas_match_single_document_census() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let generation = snapshot(all_layout_features());
    let document = generation.document();
    let mut combined = empty(document);
    for category in categories(document) {
        combined = combined
            .merge(measure(&category, &context), 0, &context, &mut |_| {
                Ok::<_, ()>(())
            })
            .unwrap();
    }
    let actual = write::measure(&generation, &context).unwrap();
    let expected = read::oracle_read_memory_plan(
        GENERATION_CONTRACT_VERSION,
        actual.stats,
        actual.maximum_payload_bytes,
        flag_rows(document),
        symbol_envelopes(document),
    )
    .unwrap();
    assert_eq!(combined.read_memory_plan(&context).unwrap(), expected);
    assert_eq!(combined.totals.entity_flag_rows, 7);
    assert_eq!(combined.totals.symbol_envelopes, 1);
    let mut peak = 0;
    let duplicate = combined
        .duplicate(17, &context, &mut |bytes| {
            peak = peak.max(bytes);
            Ok::<_, ()>(())
        })
        .unwrap();
    assert!(peak >= combined.retained_bytes().unwrap() + duplicate.retained_bytes().unwrap() + 17);
    assert_eq!(duplicate.read_memory_plan(&context).unwrap(), expected);
    assert_eq!(duplicate.totals.entity_flag_rows, 7);
    assert_eq!(duplicate.totals.symbol_envelopes, 1);
    let refused = combined.duplicate(0, &context, &mut |_| Err("duplicate workspace refused"));
    assert!(matches!(
        refused,
        Err(OracleReadAdmissionError::Admission(
            "duplicate workspace refused"
        ))
    ));
    assert_eq!(combined.read_memory_plan(&context).unwrap(), expected);
    let failure = combined.merge(duplicate, 0, &context, &mut |_| Ok::<_, ()>(()));
    assert!(
        matches!(failure, Err(OracleReadAdmissionError::Catalog(ref error))
        if error.kind() == CatalogErrorKind::InvalidGeneration)
    );
}

#[test]
fn borrowed_target_records_match_owned_writer_fragments_without_payload_clones() {
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let (document, _) = fixture_documents();
    let mut owned = NormalizedIrDocument::empty(document.repository, document.generation);
    owned.diagnostics.push(document.diagnostics[0].clone());
    let borrowed = OracleLayoutFragment::measure_diagnostic(
        &document.diagnostics[0],
        0,
        &context,
        &mut |_| Ok::<_, ()>(()),
    )
    .unwrap();
    assert_eq!(
        borrowed.read_memory_plan(&context).unwrap(),
        measure(&owned, &context)
            .read_memory_plan(&context)
            .unwrap()
    );
    owned.diagnostics.clear();
    owned.extensions.push(document.extensions[0].clone());
    let borrowed =
        OracleLayoutFragment::measure_extension(&document.extensions[0], 0, &context, &mut |_| {
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(
        borrowed.read_memory_plan(&context).unwrap(),
        measure(&owned, &context)
            .read_memory_plan(&context)
            .unwrap()
    );
    owned.extensions.clear();
    owned
        .coverage_records
        .push(document.coverage_records[0].clone());
    let borrowed = OracleLayoutFragment::measure_coverage(
        &document.coverage_records[0],
        0,
        &context,
        &mut |_| Ok::<_, ()>(()),
    )
    .unwrap();
    assert_eq!(
        borrowed.read_memory_plan(&context).unwrap(),
        measure(&owned, &context)
            .read_memory_plan(&context)
            .unwrap()
    );
}
