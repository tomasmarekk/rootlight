//! Public prefix-query behavior retaining complete original editing authority.
//!
//! Included by query_slice to reuse canonical source and conditional fixtures.

use super::*;
use rootlight_vfs::TextEncoding;

fn encode(text: &str, encoding: TextEncoding) -> Vec<u8> {
    match encoding {
        TextEncoding::Utf8 => text.as_bytes().to_vec(),
        TextEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        TextEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
    }
}

const ENCODINGS: [TextEncoding; 3] = [
    TextEncoding::Utf8,
    TextEncoding::Utf16Le,
    TextEncoding::Utf16Be,
];

fn prefix_fixture(
    path: &str,
    content: Vec<u8>,
    encoding: &str,
) -> (GenerationSnapshot, SourceSnapshot) {
    let (snapshot, source) = fallback_fixture_with_bytes(path, content, encoding);
    let mut document = snapshot.document().clone();
    document
        .files
        .iter_mut()
        .find(|file| file.id == source.file())
        .unwrap()
        .path_locator = Some(source.path().to_locator());
    let snapshot = GenerationSnapshot::new(
        snapshot.metadata(),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    (snapshot, source)
}

#[test]
fn lossless_text_prefix_preserves_context_authority_and_scalar_boundaries() {
    let text = "\u{feff}lead \"☃\"😀\r\nselected token\r\nsuffix\r\n";
    let prefix = "\u{feff}lead \"☃\"😀";
    for encoding in ENCODINGS {
        let bytes = encode(text, encoding);
        let (snapshot, retained) = prefix_fixture("context.note", bytes.clone(), encoding.as_str());
        let file = retained.file();
        let full = snapshot
            .find_file(file)
            .unwrap()
            .evidence
            .source
            .clone()
            .unwrap();
        let selected_start = text.find("selected").unwrap();
        let selected_end = selected_start + "selected".len();
        let reference = SourceRef::new(
            full.repository(),
            full.generation(),
            SourceSpan::new(
                file,
                u64::try_from(encode(&text[..selected_start], encoding).len()).unwrap(),
                u64::try_from(encode(&text[..selected_end], encoding).len()).unwrap(),
            )
            .unwrap(),
            full.content_hash(),
            None,
        );
        let search = fixture_search(&snapshot);
        let query = QueryService::new(&snapshot, &search).unwrap();
        let sources = [std::sync::Arc::new(retained)];
        let source = SourceService::from_snapshots(&sources, &snapshot).unwrap();
        let options = LosslessTextReadOptions::new()
            .with_context_lines_before(1)
            .with_context_lines_after(1);
        let plan = query
            .plan_source_read_lossless_text_prefix(
                vec![reference.clone(), reference.clone()],
                options,
                prefix.len(),
                SourceBudget::new(),
                QueryBudget::new(),
            )
            .unwrap();
        assert_eq!(plan.references(), &[reference.clone(), reference.clone()]);
        assert_eq!(plan.maximum_text_bytes_per_chunk(), prefix.len());
        assert_eq!(
            plan.explanation().estimate.source_bytes,
            u64::try_from(4 * prefix.len()).unwrap()
        );
        let response = query
            .execute_source_read_lossless_text_prefix(&plan, &source, &Cancellation::new())
            .unwrap();
        assert!(response.data.prefix_truncated);
        assert!(!response.data.result.execution.is_truncated());
        let physical_prefix = encode(prefix, encoding);
        assert_eq!(
            response.data.result.total_source_bytes,
            physical_prefix.len() * 2
        );
        assert_eq!(response.data.result.total_text_bytes, prefix.len() * 2);
        assert_eq!(
            response.usage.source_bytes,
            u64::try_from(physical_prefix.len() * 2).unwrap()
        );
        assert_eq!(response.usage.results, 2);
        assert_eq!(response.data.result.chunks.len(), 2);
        for chunk in &response.data.result.chunks {
            assert_eq!(chunk.raw().reference, reference);
            assert_eq!(chunk.raw().start_byte, 0);
            assert_eq!(
                chunk.raw().end_byte,
                u64::try_from(physical_prefix.len()).unwrap()
            );
            assert!(chunk.raw().end_byte < reference.span().start_byte());
            assert_eq!(chunk.raw().bytes, physical_prefix);
            assert_eq!(chunk.raw().content_hash, content_hash(&bytes));
            assert_eq!(chunk.text(), prefix);
            assert_eq!(chunk.physical_encoding(), encoding);
        }
        assert_exact_response_accounting(&response);
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["data"]["result"]["chunks"][0]["content"], prefix);
        assert!(json["data"]["result"]["chunks"][0].get("bytes").is_none());
        for (cap, expected) in [(1, ""), (prefix.len() - 1, "\u{feff}lead \"☃\"")] {
            let bounded = query
                .plan_source_read_lossless_text_prefix(
                    vec![reference.clone()],
                    options,
                    cap,
                    SourceBudget::new(),
                    QueryBudget::new(),
                )
                .unwrap();
            let read = query
                .execute_source_read_lossless_text_prefix(&bounded, &source, &Cancellation::new())
                .unwrap();
            assert!(read.data.prefix_truncated);
            assert_eq!(read.data.result.chunks[0].text(), expected);
            assert_eq!(
                read.data.result.chunks[0].raw().bytes,
                encode(expected, encoding)
            );
            assert_eq!(read.data.result.chunks[0].raw().reference, reference);
        }
        let complete_plan = query
            .plan_source_read_lossless_text_prefix(
                vec![reference.clone()],
                options,
                text.len(),
                SourceBudget::new(),
                QueryBudget::new(),
            )
            .unwrap();
        let complete = query
            .execute_source_read_lossless_text_prefix(&complete_plan, &source, &Cancellation::new())
            .unwrap();
        assert!(!complete.data.prefix_truncated);
        assert_eq!(complete.data.result.chunks[0].text(), text);
        let exact_plan = query
            .plan_source_read_lossless_text(
                vec![reference],
                options,
                SourceBudget::new(),
                QueryBudget::new(),
            )
            .unwrap();
        let exact = query
            .execute_source_read_lossless_text(&exact_plan, &source, &Cancellation::new())
            .unwrap();
        assert_eq!(complete.data.result, exact.data);
    }
}

#[test]
fn lossless_text_prefix_charges_physical_payload_and_both_containers() {
    let text = "\u{feff}A☃😀\r\ntail\r\n";
    let prefix = "\u{feff}A☃😀";
    for encoding in ENCODINGS {
        let bytes = encode(text, encoding);
        let (snapshot, retained) = prefix_fixture("budget.note", bytes.clone(), encoding.as_str());
        let reference = snapshot
            .find_file(retained.file())
            .unwrap()
            .evidence
            .source
            .clone()
            .unwrap();
        let search = fixture_search(&snapshot);
        let query = QueryService::new(&snapshot, &search).unwrap();
        let sources = [std::sync::Arc::new(retained)];
        let source = SourceService::from_snapshots(&sources, &snapshot).unwrap();
        let references = vec![reference.clone(), reference.clone()];
        let options = LosslessTextReadOptions::new()
            .with_context_lines_before(0)
            .with_context_lines_after(0);
        let observed = source
            .read_lossless_text_prefix_with_options(
                &references,
                options,
                prefix.len(),
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
        let payload_memory = observed.total_response_memory_bytes();
        let physical_bytes = observed.total_source_bytes();
        assert_eq!(physical_bytes, encode(prefix, encoding).len() * 2);
        let source_capacity = observed.into_chunks().capacity();
        let containers = source_capacity * std::mem::size_of::<rootlight_source::SourceTextChunk>()
            + references.len() * std::mem::size_of::<rootlight_query::QuerySourceTextChunk>();
        let total_memory = payload_memory + containers;
        let query_budget = QueryBudget::new()
            .with_max_source_bytes(u64::try_from(physical_bytes).unwrap())
            .with_max_memory_bytes(u64::try_from(total_memory).unwrap());
        let source_budget = SourceBudget::new().with_max_response_memory_bytes(payload_memory);
        let plan = query
            .plan_source_read_lossless_text_prefix(
                references.clone(),
                options,
                prefix.len(),
                source_budget,
                query_budget,
            )
            .unwrap();
        assert_eq!(
            plan.explanation().estimate.source_bytes,
            u64::try_from(physical_bytes).unwrap()
        );
        let response = query
            .execute_source_read_lossless_text_prefix(&plan, &source, &Cancellation::new())
            .unwrap();
        assert_eq!(
            response.usage.memory_bytes,
            u64::try_from(total_memory).unwrap()
        );
        assert_eq!(
            response.usage.source_bytes,
            u64::try_from(physical_bytes).unwrap()
        );
        assert_eq!(response.data.result.total_text_bytes, prefix.len() * 2);
        assert_exact_response_accounting(&response);
        let physical_short = query
            .plan_source_read_lossless_text_prefix(
                references.clone(),
                options,
                prefix.len(),
                source_budget,
                query_budget.with_max_source_bytes(u64::try_from(physical_bytes - 1).unwrap()),
            )
            .unwrap();
        assert!(matches!(
            query.execute_source_read_lossless_text_prefix(
                &physical_short,
                &source,
                &Cancellation::new()
            ),
            Err(QueryError::Source(
                rootlight_source::SourceError::SourceBudgetExceeded
            ))
        ));
        let memory_short = query
            .plan_source_read_lossless_text_prefix(
                references.clone(),
                options,
                prefix.len(),
                source_budget,
                query_budget.with_max_memory_bytes(u64::try_from(total_memory - 1).unwrap()),
            )
            .unwrap();
        assert!(matches!(
            query.execute_source_read_lossless_text_prefix(
                &memory_short,
                &source,
                &Cancellation::new()
            ),
            Err(QueryError::Source(
                rootlight_source::SourceError::ResponseMemoryBudgetExceeded
            ))
        ));
        let snapshot_short = query
            .plan_source_read_lossless_text_prefix(
                references.clone(),
                options,
                prefix.len(),
                source_budget.with_max_snapshot_bytes(u64::try_from(bytes.len() - 1).unwrap()),
                query_budget,
            )
            .unwrap();
        assert!(matches!(
            query.execute_source_read_lossless_text_prefix(
                &snapshot_short,
                &source,
                &Cancellation::new()
            ),
            Err(QueryError::Source(
                rootlight_source::SourceError::SnapshotBudgetExceeded
            ))
        ));
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            query.execute_source_read_lossless_text_prefix(&plan, &source, &cancellation),
            Err(QueryError::Cancelled(CancellationReason::ClientRequest))
        ));
        assert!(matches!(
            query.plan_source_read_lossless_text_prefix(
                references,
                options,
                0,
                source_budget,
                query_budget
            ),
            Err(QueryError::Source(
                rootlight_source::SourceError::InvalidBudget
            ))
        ));
    }
}

#[test]
fn lossless_text_prefix_projects_actual_conditional_range_and_separate_completeness() {
    let (snapshot, retained, search) = conditional_fixture();
    let sources = [retained];
    let source = SourceService::from_snapshots(&sources, &snapshot).unwrap();
    let query = QueryService::new(&snapshot, &search).unwrap();
    let located = conditional_locate(&snapshot, &search, QueryBudget::new());
    let reference = located
        .hits
        .iter()
        .find(|hit| hit.identifier == "Excluded")
        .unwrap()
        .source
        .clone()
        .unwrap();
    let options = LosslessTextReadOptions::new()
        .with_context_lines_before(1)
        .with_context_lines_after(1);
    let full_plan = query
        .plan_source_read_lossless_text_prefix(
            vec![reference.clone(), reference.clone()],
            options,
            7,
            SourceBudget::new(),
            QueryBudget::new(),
        )
        .unwrap();
    let full = query
        .execute_source_read_lossless_text_prefix(&full_plan, &source, &Cancellation::new())
        .unwrap();
    assert!(full.data.prefix_truncated);
    assert!(!full.data.result.execution.is_truncated());
    assert_eq!(full.data.result.conditional_activity.len(), 2);
    for (index, chunk) in full.data.result.chunks.iter().enumerate() {
        assert_eq!(chunk.raw().reference, reference);
        let assessment = &full.data.result.conditional_activity[index];
        assert_eq!(assessment.result_index, u32::try_from(index).unwrap());
        assert_eq!(assessment.basis, ConditionalActivityBasis::ReturnedRange);
        let assessed = assessment.assessed_source_ref.as_ref().unwrap();
        assert_eq!(assessed.span().start_byte(), chunk.raw().start_byte);
        assert_eq!(assessed.span().end_byte(), chunk.raw().end_byte);
        assert_ne!(assessed.span(), reference.span());
    }
    assert_exact_response_accounting(&full);
    let row_limited = query
        .plan_source_read_lossless_text_prefix(
            vec![reference.clone(), reference.clone()],
            options,
            7,
            SourceBudget::new(),
            QueryBudget::new().with_max_rows(2),
        )
        .unwrap();
    let limited = query
        .execute_source_read_lossless_text_prefix(&row_limited, &source, &Cancellation::new())
        .unwrap();
    assert_eq!(limited.data.result.chunks, full.data.result.chunks);
    assert!(limited.data.prefix_truncated);
    assert!(limited.data.result.execution.is_truncated());
    for assessment in &limited.data.result.conditional_activity {
        assert_eq!(
            assessment.reason,
            Some(ConditionalActivityReason::ScanIncomplete)
        );
        assert!(assessment.inventory.is_none());
    }
    let only_chunk_memory = 2
        * (std::mem::size_of::<rootlight_source::SourceTextChunk>()
            + std::mem::size_of::<rootlight_query::QuerySourceTextChunk>());
    assert!(matches!(
        query.plan_source_read_lossless_text_prefix(
            vec![reference.clone(), reference],
            options,
            7,
            SourceBudget::new(),
            QueryBudget::new().with_max_memory_bytes(u64::try_from(only_chunk_memory + 1).unwrap()),
        ),
        Err(QueryError::PlanRejected {
            resource: QueryResource::MemoryBytes
        })
    ));
}
