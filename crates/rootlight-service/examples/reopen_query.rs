//! Queries an existing durable index through public service APIs.
//! This diagnostic avoids whole-generation transfer so large repositories can
//! prove individual source-bound query trajectories without changing limits.
//! Returned inbound call evidence is read completely or fails its aggregate bound.

use std::{
    collections::BTreeSet,
    error::Error,
    ffi::OsString,
    path::PathBuf,
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, RepositoryId, SymbolId};
use rootlight_query::{QueryResponse, SymbolExplainResult, SymbolRelationshipsResult};
use rootlight_runtime::RuntimePaths;
use rootlight_service::{
    FirstSliceBudget, FirstSliceService, LocateMode, RelationDirection, RelationFamily,
    SourceEncoding, SourceReadOptions,
};
use serde_json::json;

#[path = "reopen_query/paged_source.rs"]
mod paged_source;

const MAX_CALL_EVIDENCE_READS: usize = 128;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let paged_source = paged_source_mode(&args)?;
    let state = std::fs::canonicalize(PathBuf::from(&args[0]))?;
    let repository_root = std::fs::canonicalize(PathBuf::from(&args[1]))?;
    let query = args[2].to_str().ok_or("query is not utf-8")?;
    if query.is_empty() || query.len() > 128 {
        return Err("query exceeds diagnostic bound".into());
    }
    let path_prefixes = args
        .get(3)
        .map(|path| path.to_str().ok_or("path prefix is not utf-8"))
        .transpose()?
        .map(|path| vec![path.to_owned()])
        .unwrap_or_default();
    let cancellation = Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(3600))
            .ok_or("diagnostic deadline overflow")?,
    );
    let paths = RuntimePaths::new(state.join("state"), state.join("runtime"))?;
    if !paths.state_dir().is_dir() {
        return Err("durable state directory does not exist".into());
    }
    let service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)?;
    let repository = service
        .registered_repository_for_root(&repository_root, &cancellation)?
        .ok_or("repository is not registered in the durable state")?;
    let status = service.repository_status(repository, None)?;
    let generation = status.resolved_generation;
    let located = service.code_locate_with_filters_and_budget(
        generation,
        query.to_owned(),
        LocateMode::Exact,
        Vec::new(),
        path_prefixes,
        64,
        0,
        FirstSliceBudget::default(),
        &cancellation,
    )?;
    if located.data.generation != generation {
        return Err("locate returned another generation".into());
    }
    let symbols: BTreeSet<_> = located
        .data
        .hits
        .iter()
        .filter_map(|hit| hit.symbol)
        .collect();
    if symbols.is_empty() {
        return Err("query returned no symbols".into());
    }
    if paged_source {
        let (details, source_paging) = read_paged_details(
            &service,
            status.repository,
            generation,
            symbols,
            &cancellation,
        )?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema":"rootlight-reopen-query-diagnostic/2",
                "status":{
                    "repository":status.repository,
                    "generation":status.resolved_generation,
                    "state":status.state,
                    "coverage":status.coverage.iter().map(|entry| json!({
                        "language":entry.language,
                        "discovered_files":entry.discovered_files,
                        "indexed_files":entry.indexed_files,
                        "tier":entry.tier,
                        "status":entry.status
                    })).collect::<Vec<_>>()
                },
                "locate":located,
                "details":details,
                "source_paging":source_paging,
                "full_semantic_completeness_proven":false,
                "installed_mcp_qualified":false
            }))?
        );
        return Ok(());
    }
    let mut details = Vec::new();
    let mut call_evidence_reads_total = 0_usize;
    for symbol in symbols {
        let explained = service.symbol_explain(generation, symbol, &cancellation)?;
        let related = service.symbol_relationships(
            generation,
            BTreeSet::from([symbol]),
            vec![
                RelationFamily::References,
                RelationFamily::Types,
                RelationFamily::Calls,
            ],
            Some(RelationDirection::Both),
            0,
            128,
            0,
            &cancellation,
        )?;
        if explained.data.generation != generation || related.data.generation != generation {
            return Err("symbol query returned another generation".into());
        }
        let source = explained
            .data
            .entity
            .evidence
            .source
            .clone()
            .ok_or("located symbol has no source")?;
        let read = service.source_read_with_options_and_budget(
            generation,
            vec![source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &cancellation,
        )?;
        let [chunk] = read.data.chunks.as_slice() else {
            return Err("source read returned an unexpected chunk count".into());
        };
        if read.data.generation != generation || chunk.reference != source {
            return Err("source read returned another generation or source".into());
        }
        let call_sources = related
            .data
            .groups
            .iter()
            .filter(|group| {
                group.family == RelationFamily::Calls
                    && group.direction == RelationDirection::Inbound
            })
            .flat_map(|group| &group.items)
            .flat_map(|item| &item.source_refs);
        let (call_evidence_total, call_evidence_reads) =
            read_all_call_evidence(call_sources, &mut call_evidence_reads_total, |reference| {
                let evidence = service.source_read_with_options_and_budget(
                    generation,
                    vec![reference.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(SourceEncoding::Bytes),
                    FirstSliceBudget::default(),
                    &cancellation,
                )?;
                let [chunk] = evidence.data.chunks.as_slice() else {
                    return Err("call evidence read returned an unexpected chunk count".into());
                };
                if evidence.data.generation != generation || chunk.reference != *reference {
                    return Err("call evidence read returned another generation or source".into());
                }
                Ok(evidence)
            })?;
        details.push(json!({
            "symbol":symbol,
            "explain":explained,
            "relationships":related,
            "source":read,
            "call_evidence_total":call_evidence_total,
            "call_evidence_reads":call_evidence_reads
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema":"rootlight-reopen-query-diagnostic/1",
            "status":{
                "repository":status.repository,
                "generation":status.resolved_generation,
                "state":status.state,
                "coverage":status.coverage.iter().map(|entry| json!({
                    "language":entry.language,
                    "discovered_files":entry.discovered_files,
                    "indexed_files":entry.indexed_files,
                    "tier":entry.tier,
                    "status":entry.status
                })).collect::<Vec<_>>()
            },
            "locate":located,
            "details":details,
            "full_semantic_completeness_proven":false,
            "installed_mcp_qualified":false
        }))?
    );
    Ok(())
}

fn paged_source_mode(args: &[OsString]) -> Result<bool, &'static str> {
    if !(3..=5).contains(&args.len()) || (args.len() == 5 && args[4] != "--paged-source") {
        return Err(
            "usage: reopen_query STATE_DIRECTORY REPOSITORY QUERY [PATH_PREFIX [--paged-source]]",
        );
    }
    if args.len() == 5 && args[3].is_empty() {
        return Err("paged source requires a nonempty path prefix");
    }
    Ok(args.len() == 5)
}

struct PreparedPagedDetail {
    symbol: SymbolId,
    explained: QueryResponse<SymbolExplainResult>,
    related: QueryResponse<SymbolRelationshipsResult>,
    source: paged_source::SourcePagePlan,
    calls: Vec<paged_source::SourcePagePlan>,
}

fn read_paged_details(
    service: &FirstSliceService,
    repository: RepositoryId,
    generation: GenerationId,
    symbols: BTreeSet<SymbolId>,
    cancellation: &Cancellation,
) -> Result<(Vec<serde_json::Value>, serde_json::Value), Box<dyn Error>> {
    let budget = FirstSliceBudget::default();
    let maximum_page_bytes = budget.query().max_source_bytes().min(
        u64::try_from(budget.source().max_source_bytes)
            .map_err(|_| "source page ceiling overflow")?,
    );
    let mut prepared = Vec::new();
    let mut planned_pages = 0_usize;
    let mut planned_call_references = 0_usize;
    // Query every original selector before reading any pages. A later oversized
    // group must not leave an earlier partially materialized source trajectory.
    for symbol in symbols {
        cancellation.check()?;
        let explained = service.symbol_explain(generation, symbol, cancellation)?;
        let related = service.symbol_relationships(
            generation,
            BTreeSet::from([symbol]),
            vec![
                RelationFamily::References,
                RelationFamily::Types,
                RelationFamily::Calls,
            ],
            Some(RelationDirection::Both),
            0,
            128,
            0,
            cancellation,
        )?;
        if explained.data.generation != generation || related.data.generation != generation {
            return Err("symbol query returned another generation".into());
        }
        let source = explained
            .data
            .entity
            .evidence
            .source
            .clone()
            .ok_or("located symbol has no source")?;
        let source = plan_paged_source(source, repository, generation, maximum_page_bytes)?;
        planned_pages = paged_source::checked_page_total(planned_pages, source.page_count())?;
        let call_sources = related
            .data
            .groups
            .iter()
            .filter(|group| {
                group.family == RelationFamily::Calls
                    && group.direction == RelationDirection::Inbound
            })
            .flat_map(|group| &group.items)
            .flat_map(|item| &item.source_refs);
        let count = call_sources.clone().try_fold(0_usize, |count, _| {
            count
                .checked_add(1)
                .ok_or("call evidence read count overflow")
        })?;
        planned_call_references = checked_call_evidence_total(planned_call_references, count)?;
        let mut calls = Vec::new();
        calls.try_reserve_exact(count)?;
        for reference in call_sources {
            let plan = plan_paged_source(
                reference.clone(),
                repository,
                generation,
                maximum_page_bytes,
            )?;
            planned_pages = paged_source::checked_page_total(planned_pages, plan.page_count())?;
            calls.push(plan);
        }
        prepared.push(PreparedPagedDetail {
            symbol,
            explained,
            related,
            source,
            calls,
        });
    }
    let entity_reference_count = prepared.len();
    let mut details = Vec::new();
    let mut completed_call_references = 0_usize;
    let mut completed_pages = 0_usize;
    let mut read = |reference: &rootlight_ir::SourceRef| -> Result<_, Box<dyn Error>> {
        cancellation.check()?;
        Ok(service.source_read_with_options_and_budget(
            generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            budget,
            cancellation,
        )?)
    };
    for detail in prepared {
        let source = paged_source::read_source_pages(&detail.source, &mut read)?;
        completed_pages = paged_source::checked_page_total(completed_pages, source.page_count())?;
        let (call_evidence_total, call_evidence_reads) = read_all_call_evidence(
            detail.calls.iter(),
            &mut completed_call_references,
            |plan| paged_source::read_source_pages(plan, &mut read),
        )?;
        for group in &call_evidence_reads {
            completed_pages =
                paged_source::checked_page_total(completed_pages, group.page_count())?;
        }
        details.push(json!({
            "symbol":detail.symbol,
            "explain":detail.explained,
            "relationships":detail.related,
            "source":source,
            "call_evidence_total":call_evidence_total,
            "call_evidence_reads":call_evidence_reads
        }));
    }
    if completed_pages != planned_pages || completed_call_references != planned_call_references {
        return Err("paged source did not conserve planned pages or call references".into());
    }
    Ok((
        details,
        json!({
            "maximum_page_bytes":maximum_page_bytes,
            "maximum_total_pages":paged_source::MAX_SOURCE_PAGES,
            "maximum_call_evidence_references":MAX_CALL_EVIDENCE_READS,
            "page_count":completed_pages,
            "entity_reference_count":entity_reference_count,
            "call_evidence_reference_count":completed_call_references
        }),
    ))
}

fn plan_paged_source(
    reference: rootlight_ir::SourceRef,
    repository: RepositoryId,
    generation: GenerationId,
    maximum_page_bytes: u64,
) -> Result<paged_source::SourcePagePlan, Box<dyn Error>> {
    if reference.repository() != repository || reference.generation() != generation {
        return Err("paged source reference has another repository or generation".into());
    }
    paged_source::SourcePagePlan::new(reference, maximum_page_bytes)
}

fn checked_call_evidence_total(completed: usize, additional: usize) -> Result<usize, &'static str> {
    let total = completed
        .checked_add(additional)
        .ok_or("call evidence read count overflow")?;
    if total > MAX_CALL_EVIDENCE_READS {
        return Err("call evidence reads exceed aggregate diagnostic bound");
    }
    Ok(total)
}

fn read_all_call_evidence<I, T>(
    references: I,
    completed: &mut usize,
    mut read: impl FnMut(I::Item) -> Result<T, Box<dyn Error>>,
) -> Result<(usize, Vec<T>), Box<dyn Error>>
where
    I: Iterator + Clone,
{
    let count = references.clone().try_fold(0_usize, |count, _| {
        count
            .checked_add(1)
            .ok_or("call evidence read count overflow")
    })?;
    let total = checked_call_evidence_total(*completed, count)?;
    let reads = references.map(&mut read).collect::<Result<Vec<_>, _>>()?;
    *completed = total;
    Ok((count, reads))
}

#[cfg(test)]
mod tests {
    use super::{
        checked_call_evidence_total, paged_source_mode, plan_paged_source, read_all_call_evidence,
    };

    #[test]
    fn page_planning_uses_public_default_ceilings_and_rejects_foreign_selectors() {
        let repository = rootlight_ids::RepositoryId::from_bytes([1; 16]);
        let generation = rootlight_ids::GenerationId::from_bytes([2; 20]);
        let reference = rootlight_ir::SourceRef::new(
            repository,
            generation,
            rootlight_ir::SourceSpan::new(
                rootlight_ids::FileId::from_bytes([3; 20]),
                7,
                7 + 65_537,
            )
            .unwrap(),
            rootlight_ids::content_hash(b"immutable source identity"),
            None,
        );
        let budget = rootlight_service::FirstSliceBudget::default();
        let maximum = budget
            .query()
            .max_source_bytes()
            .min(u64::try_from(budget.source().max_source_bytes).unwrap());
        assert_eq!(maximum, 65_536);
        let plan = plan_paged_source(reference.clone(), repository, generation, maximum).unwrap();
        assert_eq!(plan.page_count(), 2);
        assert!(
            plan_paged_source(
                reference.clone(),
                rootlight_ids::RepositoryId::from_bytes([9; 16]),
                generation,
                maximum
            )
            .is_err()
        );
        assert!(
            plan_paged_source(
                reference,
                repository,
                rootlight_ids::GenerationId::from_bytes([9; 20]),
                maximum
            )
            .is_err()
        );
    }

    #[test]
    fn source_paging_is_only_a_fifth_positional_opt_in() {
        let args: Vec<_> = ["state", "repository", "query"]
            .map(std::ffi::OsString::from)
            .into();
        assert_eq!(paged_source_mode(&args), Ok(false));
        let mut args = args;
        args.push("src/sample.txt".into());
        assert_eq!(paged_source_mode(&args), Ok(false));
        args.push("--paged-source".into());
        assert_eq!(paged_source_mode(&args), Ok(true));
        let prefix = args[3].clone();
        args[3] = "".into();
        assert_eq!(
            paged_source_mode(&args),
            Err("paged source requires a nonempty path prefix")
        );
        args[3] = prefix;
        args[4] = "--unknown".into();
        assert!(paged_source_mode(&args).is_err());
        args.pop();
        args[3] = "--paged-source".into();
        assert_eq!(paged_source_mode(&args), Ok(false));
        assert!(paged_source_mode(&args[..2]).is_err());
    }

    #[test]
    fn captures_every_returned_reference_beyond_sixteen_per_symbol() {
        let mut completed = 0;
        for count in [22, 62] {
            let mut observed = Vec::new();
            let (total, reads) = read_all_call_evidence(0..count, &mut completed, |reference| {
                observed.push(reference);
                Ok(reference)
            })
            .expect("both complete symbol batches fit the aggregate bound");
            let expected: Vec<_> = (0..count).collect();
            assert_eq!(total, count);
            assert_eq!(reads.len(), total);
            assert_eq!(reads, expected);
            assert_eq!(observed, expected);
        }
        assert_eq!(completed, 84);
    }

    #[test]
    fn admits_128_reads_across_multiple_symbols() {
        let mut completed = 0;
        for count in [22, 62, 44] {
            let (total, reads) = read_all_call_evidence(0..count, &mut completed, Ok)
                .expect("the complete batches fit the aggregate bound");
            assert_eq!(total, count);
            assert_eq!(reads.len(), total);
        }
        assert_eq!(completed, 128);
    }

    #[test]
    fn rejects_129_before_any_source_read_for_the_batch() {
        let mut completed = 127;
        let mut attempted = 0;
        let error = read_all_call_evidence(0..2, &mut completed, |reference| {
            attempted += 1;
            Ok(reference)
        })
        .expect_err("the batch would exceed the aggregate bound");
        assert_eq!(
            error.to_string(),
            "call evidence reads exceed aggregate diagnostic bound"
        );
        assert_eq!(attempted, 0);
        assert_eq!(completed, 127);

        let (total, reads) = read_all_call_evidence(0..1, &mut completed, Ok)
            .expect("one remaining read fits the aggregate bound");
        assert_eq!(total, 1);
        assert_eq!(reads, [0]);
        assert_eq!(completed, 128);
        assert!(
            read_all_call_evidence(0..1, &mut completed, |_| {
                attempted += 1;
                Ok(())
            })
            .is_err()
        );
        assert_eq!(attempted, 0);
        assert_eq!(completed, 128);
    }

    #[test]
    fn rejects_aggregate_read_count_overflow() {
        assert_eq!(
            checked_call_evidence_total(usize::MAX, 1),
            Err("call evidence read count overflow")
        );
    }
}
