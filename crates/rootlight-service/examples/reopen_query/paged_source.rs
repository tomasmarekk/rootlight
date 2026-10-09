//! Bounded exact-byte source pages for the opt-in reopen diagnostic.
//! Pages retain the original immutable selector and actual public responses;
//! they are not a synthetic source.read response or a new service budget.

use std::error::Error;

use rootlight_ir::{SourceRef, SourceSpan};
use rootlight_query::{PlanKind, QueryResponse, SourceChunkEncoding, SourceReadQueryResult};
use serde::Serialize;

// The diagnostic can select 64 entities and 128 inbound references. Extra pages
// are admitted only within this independent, preflighted materialization bound.
pub(super) const MAX_SOURCE_PAGES: usize = 256;

#[derive(Debug)]
/// A bounded page plan that retains the original immutable source selector.
pub(super) struct SourcePagePlan {
    reference: SourceRef,
    maximum_page_bytes: u64,
    byte_length: u64,
    page_count: usize,
}

impl SourcePagePlan {
    /// Plans nonempty exact slices without reading or allocating source bytes.
    pub(super) fn new(
        reference: SourceRef,
        maximum_page_bytes: u64,
    ) -> Result<Self, Box<dyn Error>> {
        if maximum_page_bytes == 0 {
            return Err("source page byte ceiling is zero".into());
        }
        let byte_length = reference
            .span()
            .end_byte()
            .checked_sub(reference.span().start_byte())
            .filter(|length| *length > 0)
            .ok_or("paged source reference has no written bytes")?;
        let page_count = byte_length
            .checked_div(maximum_page_bytes)
            .and_then(|count| {
                count.checked_add(u64::from(!byte_length.is_multiple_of(maximum_page_bytes)))
            })
            .ok_or("source page count overflow")?;
        let page_count = usize::try_from(page_count).map_err(|_| "source page count overflow")?;
        checked_page_total(0, page_count)?;
        Ok(Self {
            reference,
            maximum_page_bytes,
            byte_length,
            page_count,
        })
    }

    /// Returns the number of public requests required for this original selector.
    pub(super) const fn page_count(&self) -> usize {
        self.page_count
    }

    fn page_reference(&self, index: usize) -> Result<SourceRef, Box<dyn Error>> {
        if index >= self.page_count {
            return Err("source page index exceeds planned count".into());
        }
        let offset = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(self.maximum_page_bytes))
            .ok_or("source page offset overflow")?;
        let start = self
            .reference
            .span()
            .start_byte()
            .checked_add(offset)
            .ok_or("source page start overflow")?;
        let remaining = self
            .reference
            .span()
            .end_byte()
            .checked_sub(start)
            .filter(|length| *length > 0)
            .ok_or("source page is outside the original reference")?;
        let end = start
            .checked_add(remaining.min(self.maximum_page_bytes))
            .ok_or("source page end overflow")?;
        Ok(SourceRef::new(
            self.reference.repository(),
            self.reference.generation(),
            SourceSpan::new(self.reference.span().file(), start, end)?,
            self.reference.content_hash(),
            // An original whole-entity line hint does not describe a byte page.
            None,
        ))
    }
}

/// Conserves the independent aggregate request bound before any source read.
pub(super) fn checked_page_total(
    completed: usize,
    additional: usize,
) -> Result<usize, &'static str> {
    let total = completed
        .checked_add(additional)
        .ok_or("source page count overflow")?;
    if total > MAX_SOURCE_PAGES {
        return Err("source pages exceed aggregate diagnostic bound");
    }
    Ok(total)
}

#[derive(Debug, Serialize)]
/// An original selector and every actual public response that covers its bytes.
pub(super) struct SourcePageGroup {
    reference: SourceRef,
    byte_length: u64,
    page_count: usize,
    pages: Vec<QueryResponse<SourceReadQueryResult>>,
}

impl SourcePageGroup {
    /// Returns the actual number of retained public page responses.
    pub(super) const fn page_count(&self) -> usize {
        self.page_count
    }
}

/// Reads a preflighted selector, preserving every actual bounded public response.
pub(super) fn read_source_pages(
    plan: &SourcePagePlan,
    mut read: impl FnMut(&SourceRef) -> Result<QueryResponse<SourceReadQueryResult>, Box<dyn Error>>,
) -> Result<SourcePageGroup, Box<dyn Error>> {
    let mut pages = Vec::new();
    pages.try_reserve_exact(plan.page_count)?;
    let mut next_start = plan.reference.span().start_byte();
    let mut returned_bytes = 0_u64;
    for index in 0..plan.page_count {
        let reference = plan.page_reference(index)?;
        if reference.span().start_byte() != next_start {
            return Err("source page plan is not contiguous".into());
        }
        let response = read(&reference)?;
        validate_page(&reference, &response)?;
        if let Some(previous) = pages.last() {
            validate_metadata(previous, &response)?;
        }
        let length = reference
            .span()
            .end_byte()
            .checked_sub(next_start)
            .ok_or("source page byte count underflow")?;
        returned_bytes = returned_bytes
            .checked_add(length)
            .ok_or("source page byte count overflow")?;
        next_start = reference.span().end_byte();
        pages.push(response);
    }
    if next_start != plan.reference.span().end_byte()
        || returned_bytes != plan.byte_length
        || pages.len() != plan.page_count
    {
        return Err("source pages do not conserve the original reference".into());
    }
    Ok(SourcePageGroup {
        reference: plan.reference.clone(),
        byte_length: returned_bytes,
        page_count: pages.len(),
        pages,
    })
}

fn validate_page(
    reference: &SourceRef,
    response: &QueryResponse<SourceReadQueryResult>,
) -> Result<(), Box<dyn Error>> {
    let [chunk] = response.data.chunks.as_slice() else {
        return Err("source page returned an unexpected chunk count".into());
    };
    let length = reference
        .span()
        .end_byte()
        .checked_sub(reference.span().start_byte())
        .ok_or("source page byte count underflow")?;
    if response.plan.kind != PlanKind::SourceRead
        || response.plan.generation != reference.generation()
        || response.data.generation != reference.generation()
        || !response.data.execution.is_complete()
        || chunk.reference != *reference
        || chunk.start_byte != reference.span().start_byte()
        || chunk.end_byte != reference.span().end_byte()
        || chunk.content_hash != reference.content_hash()
        || chunk.encoding != SourceChunkEncoding::Bytes
        || chunk.start_line.is_some()
        || chunk.end_line.is_some()
        || u64::try_from(chunk.bytes.len()).ok() != Some(length)
        || response.usage.source_bytes != length
        || response.usage.results != 1
    {
        return Err("source page does not match its exact immutable selector".into());
    }
    Ok(())
}

fn validate_metadata(
    previous: &QueryResponse<SourceReadQueryResult>,
    current: &QueryResponse<SourceReadQueryResult>,
) -> Result<(), Box<dyn Error>> {
    let ([before], [after]) = (
        previous.data.chunks.as_slice(),
        current.data.chunks.as_slice(),
    ) else {
        return Err("source page returned an unexpected chunk count".into());
    };
    if before.path != after.path
        || before.language != after.language
        || before.generated != after.generated
        || before.trust != after.trust
        || before.extracted_from != after.extracted_from
    {
        return Err("source page metadata changed within the original reference".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, GenerationId, RepositoryId, content_hash};
    use rootlight_ir::LineRange;
    use rootlight_query::{
        ExecutionCompleteness, PlanEstimate, PlanExplanation, QueryResource, QueryUsage,
        RepositoryDataTrust, SourceChunkResult, TokenAccountingProfile,
    };

    const PAGE_BYTES: u64 = 64 * 1024;

    fn reference(start: u64, end: u64) -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(FileId::from_bytes([3; 20]), start, end).unwrap(),
            content_hash(b"whole immutable file, not a page hash"),
            Some(LineRange::new(2, 9).unwrap()),
        )
    }

    fn response(reference: &SourceRef, bytes: Vec<u8>) -> QueryResponse<SourceReadQueryResult> {
        let length = u64::try_from(bytes.len()).unwrap();
        QueryResponse {
            plan: PlanExplanation {
                generation: reference.generation(),
                kind: PlanKind::SourceRead,
                operators: Vec::new(),
                estimate: PlanEstimate {
                    rows: 1,
                    edges: 0,
                    results: 1,
                    source_bytes: PAGE_BYTES,
                    memory_bytes: 1_000_000,
                    json_bytes: 1_000_000,
                    estimated_tokens: 1_000_000,
                    duration_micros: 1_000,
                },
            },
            data: SourceReadQueryResult {
                generation: reference.generation(),
                chunks: vec![SourceChunkResult {
                    extracted_from: None,
                    reference: reference.clone(),
                    path: "src/sample.txt".into(),
                    start_byte: reference.span().start_byte(),
                    end_byte: reference.span().end_byte(),
                    start_line: None,
                    end_line: None,
                    bytes,
                    encoding: SourceChunkEncoding::Bytes,
                    content_hash: reference.content_hash(),
                    language: "text".into(),
                    generated: false,
                    trust: RepositoryDataTrust::UntrustedRepositoryData,
                }],
                conditional_activity: Vec::new(),
                execution: ExecutionCompleteness::complete(),
            },
            usage: QueryUsage {
                rows: 1,
                edges: 0,
                results: 1,
                source_bytes: length,
                json_bytes: 0,
                estimated_tokens: 0,
                token_accounting: TokenAccountingProfile::Utf8ByteUpperBoundV1,
                memory_bytes: length,
                elapsed_micros: 1,
            },
        }
    }

    #[test]
    fn exact_boundary_and_remainder_pages_preserve_original_identity() {
        for (length, count) in [(PAGE_BYTES - 1, 1), (PAGE_BYTES, 1), (PAGE_BYTES + 1, 2)] {
            let original = reference(19, 19 + length);
            let plan = SourcePagePlan::new(original.clone(), PAGE_BYTES).unwrap();
            assert_eq!(plan.page_count(), count);
            let mut next = 19;
            for index in 0..count {
                let page = plan.page_reference(index).unwrap();
                assert_eq!(page.span().start_byte(), next);
                assert!(page.span().end_byte() - next <= PAGE_BYTES);
                assert_eq!(page.repository(), original.repository());
                assert_eq!(page.generation(), original.generation());
                assert_eq!(page.span().file(), original.span().file());
                assert_eq!(page.content_hash(), original.content_hash());
                assert_eq!(page.line_hint(), None);
                next = page.span().end_byte();
            }
            assert_eq!(next, original.span().end_byte());
            assert!(plan.page_reference(count).is_err());
            assert_eq!(plan.reference, original);
        }
    }

    #[test]
    fn empty_spans_zero_ceilings_and_excessive_plans_are_rejected() {
        assert!(SourcePagePlan::new(reference(9, 9), PAGE_BYTES).is_err());
        assert!(SourcePagePlan::new(reference(9, 10), 0).is_err());
        let beyond = u64::try_from(MAX_SOURCE_PAGES + 1).unwrap() * PAGE_BYTES;
        assert!(SourcePagePlan::new(reference(0, beyond), PAGE_BYTES).is_err());
        assert_eq!(
            checked_page_total(usize::MAX, 1),
            Err("source page count overflow")
        );
    }

    #[test]
    fn offsets_near_u64_end_and_large_ceilings_do_not_overflow() {
        let plan = SourcePagePlan::new(reference(u64::MAX - 3, u64::MAX), 2).unwrap();
        assert_eq!(plan.page_count(), 2);
        assert_eq!(plan.page_reference(1).unwrap().span().end_byte(), u64::MAX);
        let plan = SourcePagePlan::new(reference(0, u64::MAX), u64::MAX).unwrap();
        assert_eq!(plan.page_count(), 1);
        assert_eq!(plan.page_reference(0).unwrap().span().end_byte(), u64::MAX);
    }

    #[test]
    fn aggregate_preflight_rejects_before_any_read() {
        let plans = [
            SourcePagePlan::new(reference(0, PAGE_BYTES), PAGE_BYTES).unwrap(),
            SourcePagePlan::new(reference(0, 2 * PAGE_BYTES), PAGE_BYTES).unwrap(),
        ];
        let mut attempted = 0;
        let result = plans.iter().try_fold(MAX_SOURCE_PAGES - 2, |count, plan| {
            checked_page_total(count, plan.page_count())
        });
        if result.is_ok() {
            read_source_pages(&plans[0], |page| {
                attempted += 1;
                Ok(response(
                    page,
                    vec![0; usize::try_from(PAGE_BYTES).unwrap()],
                ))
            })
            .unwrap();
        }
        assert!(result.is_err());
        assert_eq!(attempted, 0);
    }

    #[test]
    fn actual_pages_conserve_uninterpreted_bytes_and_do_not_merge_responses() {
        let original = reference(13, 13 + PAGE_BYTES + 3);
        let plan = SourcePagePlan::new(original.clone(), PAGE_BYTES).unwrap();
        let bytes: Vec<u8> = (0..usize::try_from(PAGE_BYTES + 3).unwrap())
            .map(|index| u8::try_from(index % 256).unwrap())
            .collect();
        let mut actual = Vec::new();
        let group = read_source_pages(&plan, |page| {
            let start = usize::try_from(page.span().start_byte() - 13).unwrap();
            let end = usize::try_from(page.span().end_byte() - 13).unwrap();
            let result = response(page, bytes[start..end].to_vec());
            actual.push(result.clone());
            Ok(result)
        })
        .unwrap();
        assert_eq!(group.reference, original);
        assert_eq!(group.byte_length, PAGE_BYTES + 3);
        assert_eq!(group.page_count, 2);
        assert_eq!(group.pages, actual);
        assert_eq!(
            group
                .pages
                .iter()
                .flat_map(|page| &page.data.chunks[0].bytes)
                .copied()
                .collect::<Vec<_>>(),
            bytes
        );
        let wire = serde_json::to_value(&group).unwrap();
        assert!(wire.get("data").is_none());
        assert_eq!(wire["pages"].as_array().unwrap().len(), 2);
        assert_eq!(
            wire["reference"],
            serde_json::to_value(group.reference).unwrap()
        );
    }

    #[test]
    fn mismatched_page_identity_ranges_hash_encoding_and_usage_fail_closed() {
        let plan = SourcePagePlan::new(reference(3, 6), PAGE_BYTES).unwrap();
        for mutation in 0..9 {
            let result = read_source_pages(&plan, |page| {
                let mut result = response(page, vec![1, 2, 3]);
                match mutation {
                    0 => result.data.generation = GenerationId::from_bytes([9; 20]),
                    1 => result.plan.generation = GenerationId::from_bytes([9; 20]),
                    2 => result.data.chunks[0].reference = reference(3, 5),
                    3 => result.data.chunks[0].start_byte = 2,
                    4 => result.data.chunks[0].end_byte = 7,
                    5 => result.data.chunks[0].content_hash = content_hash(b"other file"),
                    6 => result.data.chunks[0].encoding = SourceChunkEncoding::Utf8,
                    7 => assert!(result.data.chunks[0].bytes.pop().is_some()),
                    8 => result.usage.source_bytes = 2,
                    _ => unreachable!(),
                }
                Ok(result)
            });
            assert!(result.is_err(), "mutation {mutation}");
        }
    }

    #[test]
    fn partial_execution_extra_chunks_and_changed_metadata_fail_closed() {
        let plan = SourcePagePlan::new(reference(0, PAGE_BYTES + 1), PAGE_BYTES).unwrap();
        for mutation in 0..4 {
            let result = read_source_pages(&plan, |page| {
                let length =
                    usize::try_from(page.span().end_byte() - page.span().start_byte()).unwrap();
                let mut result = response(page, vec![0; length]);
                if page.span().start_byte() > 0 {
                    match mutation {
                        0 => {
                            result.data.execution =
                                ExecutionCompleteness::truncated(QueryResource::Rows, [])
                        }
                        1 => result.data.chunks.push(result.data.chunks[0].clone()),
                        2 => result.data.chunks[0].path = "other.txt".into(),
                        3 => result.data.chunks[0].extracted_from = Some(reference(0, 3)),
                        _ => unreachable!(),
                    }
                }
                Ok(result)
            });
            assert!(result.is_err(), "mutation {mutation}");
        }
    }

    #[test]
    fn a_failed_page_stops_without_a_partial_group_or_later_reads() {
        let plan = SourcePagePlan::new(reference(0, 3 * PAGE_BYTES), PAGE_BYTES).unwrap();
        let mut attempted = 0;
        let result = read_source_pages(&plan, |page| {
            attempted += 1;
            if attempted == 2 {
                return Err("injected public source read failure".into());
            }
            let length =
                usize::try_from(page.span().end_byte() - page.span().start_byte()).unwrap();
            Ok(response(page, vec![0; length]))
        });
        assert_eq!(
            result.unwrap_err().to_string(),
            "injected public source read failure"
        );
        assert_eq!(attempted, 2);
    }
}
