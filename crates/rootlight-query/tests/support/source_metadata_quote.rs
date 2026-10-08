//! Metadata-only quote integration with real source and conditional fixtures.
//! The oracles compare future bound components to actual prefix reads without
//! treating a successful quote as source-body or scalar-boundary validation.

use super::*;

fn options() -> LosslessTextReadOptions {
    LosslessTextReadOptions::new()
        .with_context_lines_before(0)
        .with_context_lines_after(0)
}

#[test]
fn source_metadata_quote_preserves_order_and_conserves_actual_prefix_costs() {
    let path = format!("{}/escaped\".note", "long-segment".repeat(160));
    let (template, retained) =
        fallback_fixture_with_bytes(&path, b"text payload\n".to_vec(), "utf-8");
    let mut document = template.document().clone();
    document
        .files
        .iter_mut()
        .find(|file| file.id == retained.file())
        .unwrap()
        .path_locator = Some(retained.path().to_locator());
    let snapshot = GenerationSnapshot::new(
        template.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let full = snapshot
        .find_file(retained.file())
        .unwrap()
        .evidence
        .source
        .clone()
        .unwrap();
    let shorter = SourceRef::new(
        full.repository(),
        full.generation(),
        SourceSpan::new(full.span().file(), 1, 8).unwrap(),
        full.content_hash(),
        None,
    );
    let references = [full.clone(), shorter];
    let search = fixture_search(&snapshot);
    let query = QueryService::new(&snapshot, &search).unwrap();
    let quoted = query
        .source_text_prefix_metadata_quote(
            &references,
            options(),
            SourceBudget::new(),
            QueryBudget::new(),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(quoted.data.quote_policy_version, 1);
    assert_eq!(quoted.usage.source_bytes, 0);
    assert!(quoted.usage.rows > 2);
    assert!(quoted.data.execution.is_complete());
    assert!(
        !quoted
            .plan
            .operators
            .contains(&rootlight_query::QueryOperator::VfsSnapshotRead)
    );
    assert!(
        !quoted
            .plan
            .operators
            .contains(&rootlight_query::QueryOperator::ContentHashVerify)
    );
    let sources = [Arc::new(retained)];
    let source = SourceService::from_snapshots(&sources, &snapshot).unwrap();
    for (index, entry) in quoted.data.entries.iter().enumerate() {
        assert_eq!(entry.reference, references[index]);
        assert_eq!(
            entry.prefix_fixed_upper.paths,
            u64::try_from(index + 1).unwrap()
        );
        let plan = query
            .plan_source_read_lossless_text_prefix(
                references[..=index].to_vec(),
                options(),
                4,
                SourceBudget::new(),
                QueryBudget::new(),
            )
            .unwrap();
        let actual = query
            .execute_source_read_lossless_text_prefix(&plan, &source, &Cancellation::new())
            .unwrap();
        let upper = entry.prefix_fixed_upper;
        assert!(
            actual.usage.json_bytes <= upper.json_bytes + 6 * 4 * u64::try_from(index + 1).unwrap()
        );
        assert!(
            actual.usage.memory_bytes
                <= upper.memory_bytes + 3 * 4 * u64::try_from(index + 1).unwrap()
        );
        assert!(actual.usage.rows <= upper.rows);
        if index != 0 {
            let prior = quoted.data.entries[index - 1].prefix_fixed_upper;
            assert!(
                upper.json_bytes >= prior.json_bytes
                    && upper.memory_bytes >= prior.memory_bytes
                    && upper.rows >= prior.rows
                    && upper.traversal_facts >= prior.traversal_facts
            );
        }
    }
    assert_exact_response_accounting(&quoted);
}

#[test]
fn source_metadata_quote_bounds_narrow_conditional_inventory_and_decoder_scratch() {
    let (snapshot, retained, search) = conditional_fixture();
    let full = snapshot
        .find_file(retained.file())
        .unwrap()
        .evidence
        .source
        .clone()
        .unwrap();
    let query = QueryService::new(&snapshot, &search).unwrap();
    let quote = query
        .source_text_prefix_metadata_quote(
            std::slice::from_ref(&full),
            options(),
            SourceBudget::new(),
            QueryBudget::new(),
            &Cancellation::new(),
        )
        .unwrap();
    let upper = quote.data.entries[0].prefix_fixed_upper;
    assert!(
        upper.memory_bytes > 8_192,
        "stored decoder scratch is not a fixed small sidecar allowance"
    );
    assert!(upper.traversal_facts > 1);
    let sources = [retained];
    let source = SourceService::from_snapshots(&sources, &snapshot).unwrap();
    let token = snapshot
        .document()
        .occurrences
        .iter()
        .find(|record| record.role == OccurrenceRole::Definition)
        .unwrap()
        .source
        .clone();
    for reference in [full, token] {
        let quoted = query
            .source_text_prefix_metadata_quote(
                std::slice::from_ref(&reference),
                options(),
                SourceBudget::new(),
                QueryBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
        let plan = query
            .plan_source_read_lossless_text_prefix(
                vec![reference],
                options(),
                64,
                SourceBudget::new(),
                QueryBudget::new(),
            )
            .unwrap();
        let actual = query
            .execute_source_read_lossless_text_prefix(&plan, &source, &Cancellation::new())
            .unwrap();
        let bound = quoted.data.entries[0].prefix_fixed_upper;
        assert!(actual.usage.json_bytes <= bound.json_bytes + 6 * 64);
        assert!(actual.usage.memory_bytes <= bound.memory_bytes + 3 * 64);
        assert!(actual.usage.rows <= bound.rows);
        assert!(!actual.data.result.conditional_activity.is_empty());
    }
}

#[test]
fn source_metadata_quote_is_body_free_but_rejects_budget_and_identity_failures() {
    let (template, retained) =
        fallback_fixture_with_bytes("invalid.note", vec![0xff, 0xfe, 0x61], "utf-16le");
    let mut document = template.document().clone();
    document
        .files
        .iter_mut()
        .find(|file| file.id == retained.file())
        .unwrap()
        .path_locator = Some(retained.path().to_locator());
    let snapshot = GenerationSnapshot::new(
        template.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let full = snapshot
        .find_file(retained.file())
        .unwrap()
        .evidence
        .source
        .clone()
        .unwrap();
    drop(retained);
    let search = fixture_search(&snapshot);
    let query = QueryService::new(&snapshot, &search).unwrap();
    let first = query
        .source_text_prefix_metadata_quote(
            std::slice::from_ref(&full),
            options(),
            SourceBudget::new(),
            QueryBudget::new(),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(first.usage.source_bytes, 0);
    let changed = SourceRef::new(
        full.repository(),
        full.generation(),
        full.span(),
        content_hash(b"different"),
        None,
    );
    assert!(
        query
            .source_text_prefix_metadata_quote(
                &[changed],
                options(),
                SourceBudget::new(),
                QueryBudget::new(),
                &Cancellation::new()
            )
            .is_err()
    );
    let constrained = QueryBudget::new().with_max_rows(first.usage.rows - 1);
    assert!(matches!(
        query.source_text_prefix_metadata_quote(
            std::slice::from_ref(&full),
            options(),
            SourceBudget::new(),
            constrained,
            &Cancellation::new()
        ),
        Err(QueryError::BudgetExceeded {
            resource: QueryResource::Rows,
            ..
        })
    ));
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        query.source_text_prefix_metadata_quote(
            &[full],
            options(),
            SourceBudget::new(),
            QueryBudget::new(),
            &cancellation
        ),
        Err(QueryError::Cancelled(_))
    ));
}
