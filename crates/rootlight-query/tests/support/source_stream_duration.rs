//! Public source-query deadline provenance over a deliberately slow capability.
//! Default omission must survive both query and source planning, while an
//! explicit deadline equal to the numeric default still limits the same visit.

use super::*;
use rootlight_source::{SourceError, VerifiedSourceBacking};
use std::cell::Cell;

struct SlowBacking<'a> {
    generation: &'a GenerationSnapshot,
    source: &'a SourceSnapshot,
    delay: Duration,
    calls: Cell<usize>,
}

impl std::fmt::Debug for SlowBacking<'_> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("SlowBacking").finish_non_exhaustive()
    }
}

impl VerifiedSourceBacking for SlowBacking<'_> {
    fn repository(&self) -> RepositoryId {
        self.generation.metadata().repository()
    }
    fn generation(&self) -> rootlight_ids::GenerationId {
        self.generation.metadata().generation()
    }
    fn visit_verified(
        &self,
        file: &FileRecord,
        maximum: u64,
        cancellation: &Cancellation,
        visitor: &mut dyn FnMut(u64, &[u8]) -> Result<(), SourceError>,
    ) -> Result<u64, SourceError> {
        self.calls.set(self.calls.get() + 1);
        cancellation
            .check()
            .map_err(|error| SourceError::Cancelled(error.reason()))?;
        if self.generation.find_file(file.id) != Some(file)
            || self.source.file() != file.id
            || self.source.content_hash() != file.content_hash
            || self.source.metadata().length != file.byte_length
        {
            return Err(SourceError::StaleSource);
        }
        if file.byte_length > maximum {
            return Err(SourceError::VerificationBudgetExceeded);
        }
        visitor(0, self.source.content())?;
        std::thread::sleep(self.delay);
        Ok(file.byte_length)
    }
}

#[test]
fn streamed_source_query_omission_and_explicit_default_duration_are_distinct() {
    let (old, retained) = fallback_fixture_with_bytes("duration.note", b"text".to_vec(), "utf-8");
    let mut document = old.document().clone();
    document
        .files
        .iter_mut()
        .find(|file| file.id == retained.file())
        .unwrap()
        .path_locator = Some(retained.path().to_locator());
    let snapshot = GenerationSnapshot::new(
        old.metadata(),
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
    let backing = SlowBacking {
        generation: &snapshot,
        source: &retained,
        delay: Duration::from_millis(2050),
        calls: Cell::new(0),
    };
    let source = SourceService::from_verified_backing(&backing, &snapshot).unwrap();
    let search = fixture_search(&snapshot);
    let query = QueryService::new(&snapshot, &search).unwrap();
    let options = SourceReadOptions::new()
        .with_context_lines_before(0)
        .with_context_lines_after(0)
        .with_encoding(rootlight_source::SourceEncoding::Bytes);
    let plan = query
        .plan_source_read(
            vec![full.clone()],
            options,
            SourceBudget::new(),
            QueryBudget::new(),
        )
        .unwrap();
    let response = query
        .execute_source_read(&plan, &source, &Cancellation::new())
        .unwrap();
    assert_eq!(response.data.chunks[0].bytes, b"text");
    assert!(response.usage.elapsed_micros >= 2_000_000);
    for (source_budget, query_budget) in [
        (
            SourceBudget::new(),
            QueryBudget::new().with_max_duration(QueryBudget::new().max_duration()),
        ),
        (
            SourceBudget::new().with_max_duration(SourceBudget::new().max_duration),
            QueryBudget::new(),
        ),
    ] {
        let plan = query
            .plan_source_read(vec![full.clone()], options, source_budget, query_budget)
            .unwrap();
        assert!(matches!(
            query.execute_source_read(&plan, &source, &Cancellation::new()),
            Err(QueryError::Source(SourceError::Cancelled(
                CancellationReason::DeadlineExceeded
            ))) | Err(QueryError::Cancelled(CancellationReason::DeadlineExceeded))
        ));
    }
    assert_eq!(backing.calls.get(), 3);
    let plan = query
        .plan_source_read(
            vec![full],
            options,
            SourceBudget::new(),
            QueryBudget::new().with_max_duration(Duration::from_nanos(1)),
        )
        .unwrap();
    assert!(matches!(
        query.execute_source_read(&plan, &source, &Cancellation::new()),
        Err(QueryError::Cancelled(CancellationReason::DeadlineExceeded))
    ));
    assert_eq!(backing.calls.get(), 3);
    let cancellation = Cancellation::new();
    let _ = cancellation.cancel(CancellationReason::ParentCancelled);
    assert!(matches!(
        query.execute_source_read(&plan, &source, &cancellation),
        Err(QueryError::Cancelled(CancellationReason::ParentCancelled))
    ));
    assert_eq!(backing.calls.get(), 3);
}
