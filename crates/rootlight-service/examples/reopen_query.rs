//! Queries an existing durable index through public service APIs.
//! This diagnostic avoids whole-generation transfer so large repositories can
//! prove individual source-bound query trajectories without changing limits.
//! Returned inbound call evidence is read completely or fails its aggregate bound.
//! JSONL requests share one restored service and can select retained generations.
//! Only a successful footer qualifies completion of the entire streamed batch.

use std::{
    collections::BTreeSet,
    error::Error,
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
};

use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, RepositoryId, SymbolId};
use rootlight_query::{QueryResponse, SymbolExplainResult, SymbolRelationshipsResult};
use rootlight_runtime::RuntimePaths;
use rootlight_service::{
    FirstSliceBudget, FirstSliceService, LocateMode, RelationDirection, RelationFamily,
    SourceEncoding, SourceReadOptions, with_first_slice_locate_profile,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[path = "reopen_query/literal_find.rs"]
mod literal_find;
#[path = "reopen_query/paged_source.rs"]
mod paged_source;

const MAX_CALL_EVIDENCE_READS: usize = 128;
// JSON can expand one admitted literal byte to six escaped ASCII bytes. Keep
// the existing metadata allowance in addition to a complete maximum literal.
const MAX_BATCH_REQUEST_LINE_BYTES: usize = 6 * rootlight_source::SourceFindBudget::new()
    .matcher
    .max_query_bytes
    + 64 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum QueryMode {
    #[default]
    Locate,
    Text,
    SourceFind,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct QueryRequest {
    query: String,
    #[serde(default)]
    mode: QueryMode,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    path_prefix: Option<String>,
    #[serde(default)]
    generation: Option<GenerationId>,
    #[serde(default)]
    paged_source: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    find: Option<literal_find::FindRequest>,
}

impl QueryRequest {
    fn validate(&self) -> Result<(), &'static str> {
        if self.mode == QueryMode::SourceFind {
            return literal_find::validate(self);
        }
        if self.find.is_some() {
            return Err("literal selector requires source_find mode");
        }
        if self.query.is_empty() || self.query.len() > 128 {
            return Err("query exceeds diagnostic bound");
        }
        if self.paged_source && self.path_prefix.as_ref().is_none_or(String::is_empty) {
            return Err("paged source requires a nonempty path prefix");
        }
        if self.paged_source && self.mode != QueryMode::Locate {
            return Err("paged source requires locate mode");
        }
        Ok(())
    }
}

fn next_batch_request(
    input: &mut impl BufRead,
    line: &mut Vec<u8>,
) -> Result<Option<QueryRequest>, Box<dyn Error>> {
    line.clear();
    let bytes = input
        .take(u64::try_from(MAX_BATCH_REQUEST_LINE_BYTES + 1)?)
        .read_until(b'\n', line)?;
    if bytes == 0 {
        return Ok(None);
    }
    if bytes > MAX_BATCH_REQUEST_LINE_BYTES {
        return Err("batch request line exceeds diagnostic bound".into());
    }
    let request: QueryRequest = serde_json::from_slice(line)?;
    request.validate()?;
    Ok(Some(request))
}

fn run_batch(
    input: &mut impl BufRead,
    output: &mut impl Write,
    mut query: impl FnMut(&QueryRequest) -> Result<Value, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let mut completed_requests = 0_u64;
    let mut line = Vec::new();
    let result = (|| -> Result<(), Box<dyn Error>> {
        while let Some(request) = next_batch_request(input, &mut line)? {
            let next = completed_requests
                .checked_add(1)
                .ok_or("batch completed request count overflow")?;
            let result = query(&request)?;
            serde_json::to_writer(
                &mut *output,
                &json!({
                    "schema":"rootlight-reopen-query-batch-entry/1",
                    "index":completed_requests,
                    "request":request,
                    "result":result
                }),
            )?;
            writeln!(output)?;
            output.flush()?;
            completed_requests = next;
        }
        if completed_requests == 0 {
            return Err("batch contains no requests".into());
        }
        Ok(())
    })();
    // A prefix is useful evidence but cannot stand in for a completed batch.
    let footer = (|| -> Result<(), Box<dyn Error>> {
        serde_json::to_writer(
            &mut *output,
            &json!({
                "schema":"rootlight-reopen-query-batch-complete/1",
                "completed_requests":completed_requests,
                "complete":result.is_ok()
            }),
        )?;
        writeln!(output)?;
        output.flush()?;
        Ok(())
    })();
    result.and(footer)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let (args, mut profile) = if args.first().is_some_and(|arg| arg == "--profile") {
        if args.len() != 6 || args.get(2).is_none_or(|arg| arg != "--batch") {
            return Err("usage: reopen_query --profile OBSERVATIONS_JSONL --batch STATE_DIRECTORY REPOSITORY REQUESTS_JSONL".into());
        }
        let output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(PathBuf::from(&args[1]))?;
        (&args[2..], Some(output))
    } else {
        (&args[..], None)
    };
    if args.first().is_some_and(|arg| arg == "--batch") {
        if args.len() != 4 {
            return Err(
                "usage: reopen_query --batch STATE_DIRECTORY REPOSITORY REQUESTS_JSONL".into(),
            );
        }
        let mut input = BufReader::new(std::fs::File::open(PathBuf::from(&args[3]))?);
        let cancellation = Cancellation::new();
        let (service, repository) = if let Some(output) = profile.as_mut() {
            let started = std::time::Instant::now();
            let result = open_service(&args[1], &args[2], &cancellation);
            serde_json::to_writer(
                &mut *output,
                &json!({
                    "schema":"rootlight-reopen-open-observation/1",
                    "wall_micros":started.elapsed().as_micros(),
                    "operation_succeeded":result.is_ok(),
                }),
            )?;
            writeln!(output)?;
            output.flush()?;
            result?
        } else {
            open_service(&args[1], &args[2], &cancellation)?
        };
        let mut stdout = std::io::stdout().lock();
        let mut attempted = 0_u64;
        return run_batch(&mut input, &mut stdout, |request| {
            let Some(output) = profile.as_mut() else {
                return query_existing(&service, repository, request, true, &cancellation);
            };
            let (result, observation) = with_first_slice_locate_profile(|| {
                query_existing(&service, repository, request, true, &cancellation)
            });
            serde_json::to_writer(
                &mut *output,
                &json!({
                    "schema":"rootlight-reopen-locate-observation/1",
                    "attempted_request":attempted,
                    "operation_succeeded":result.is_ok(),
                    "observation":observation,
                }),
            )?;
            writeln!(output)?;
            output.flush()?;
            attempted = attempted
                .checked_add(1)
                .ok_or("observation ordinal overflow")?;
            result
        });
    }
    let paged_source = paged_source_mode(args)?;
    let request = QueryRequest {
        query: args[2].to_str().ok_or("query is not utf-8")?.to_owned(),
        mode: QueryMode::Locate,
        offset: 0,
        path_prefix: args
            .get(3)
            .map(|path| path.to_str().ok_or("path prefix is not utf-8"))
            .transpose()?
            .map(str::to_owned),
        generation: None,
        paged_source,
        find: None,
    };
    request.validate()?;
    let cancellation = Cancellation::new();
    let (service, repository) = open_service(&args[0], &args[1], &cancellation)?;
    let result = query_existing(&service, repository, &request, false, &cancellation)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn open_service(
    state: &OsString,
    repository_root: &OsString,
    cancellation: &Cancellation,
) -> Result<(FirstSliceService, RepositoryId), Box<dyn Error>> {
    let state = std::fs::canonicalize(PathBuf::from(state))?;
    let repository_root = std::fs::canonicalize(PathBuf::from(repository_root))?;
    let paths = RuntimePaths::new(state.join("state"), state.join("runtime"))?;
    if !paths.state_dir().is_dir() {
        return Err("durable state directory does not exist".into());
    }
    let service = FirstSliceService::new_durable(2, paths.state_dir(), cancellation)?;
    let repository = service
        .registered_repository_for_root(&repository_root, cancellation)?
        .ok_or("repository is not registered in the durable state")?;
    Ok((service, repository))
}

fn query_existing(
    service: &FirstSliceService,
    repository: RepositoryId,
    request: &QueryRequest,
    allow_symbol_free: bool,
    cancellation: &Cancellation,
) -> Result<Value, Box<dyn Error>> {
    request.validate()?;
    let status = service.repository_status(repository, request.generation)?;
    let generation = status.resolved_generation;
    if request.mode == QueryMode::SourceFind {
        return literal_find::query(
            service,
            status.repository,
            generation,
            request,
            cancellation,
        );
    }
    let located = service.code_locate_with_filters_and_budget(
        generation,
        request.query.clone(),
        match request.mode {
            QueryMode::Locate => LocateMode::Exact,
            QueryMode::Text => LocateMode::Text,
            QueryMode::SourceFind => return Err("literal mode reached lexical planning".into()),
        },
        Vec::new(),
        request.path_prefix.iter().cloned().collect(),
        64,
        request.offset,
        FirstSliceBudget::default(),
        cancellation,
    )?;
    if located.data.generation != generation {
        return Err("locate returned another generation".into());
    }
    if request.mode == QueryMode::Text {
        return Ok(json!({
            "schema":"rootlight-reopen-text-query-diagnostic/1",
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
            "details":[],
            "full_semantic_completeness_proven":false,
            "installed_mcp_qualified":false
        }));
    }
    let symbols: BTreeSet<_> = located
        .data
        .hits
        .iter()
        .filter_map(|hit| hit.symbol)
        .collect();
    if symbols.is_empty() && !allow_symbol_free {
        return Err("query returned no symbols".into());
    }
    if request.paged_source {
        let (details, source_paging) = read_paged_details(
            service,
            status.repository,
            generation,
            symbols,
            cancellation,
        )?;
        return Ok(json!({
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
        }));
    }
    let mut details = Vec::new();
    let mut call_evidence_reads_total = 0_usize;
    for symbol in symbols {
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
        let read = service.source_read_with_options_and_budget(
            generation,
            vec![source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            cancellation,
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
                    cancellation,
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
    Ok(json!({
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
    }))
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
    use std::io::{self, BufRead, Read, Write};

    use super::{
        QueryMode, QueryRequest, checked_call_evidence_total, next_batch_request,
        paged_source_mode, plan_paged_source, query_existing, read_all_call_evidence, run_batch,
    };

    fn output_records(output: &[u8]) -> Vec<serde_json::Value> {
        output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect()
    }

    struct FailAfterPrefix {
        prefix: std::io::Cursor<Vec<u8>>,
    }

    impl Read for FailAfterPrefix {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let available = self.fill_buf()?;
            let count = available.len().min(output.len());
            output[..count].copy_from_slice(&available[..count]);
            self.consume(count);
            Ok(count)
        }
    }

    impl BufRead for FailAfterPrefix {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            let available = self.prefix.fill_buf()?;
            if available.is_empty() {
                Err(io::Error::other("input read failed"))
            } else {
                Ok(available)
            }
        }

        fn consume(&mut self, amount: usize) {
            self.prefix.consume(amount);
        }
    }

    #[derive(Default)]
    struct FailFlush {
        bytes: Vec<u8>,
    }

    impl Write for FailFlush {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(input);
            Ok(input.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("output flush failed"))
        }
    }

    #[test]
    fn batch_requests_preserve_explicit_generation_and_source_modes() {
        let generation = rootlight_ids::GenerationId::from_bytes([2; 20]);
        let raw = format!(
            "{}\r\n{}",
            serde_json::json!({"query":"Current","path_prefix":"src/current.rs"}),
            serde_json::json!({"query":"Retained","generation":generation,"path_prefix":"src/retained.rs","paged_source":true,"offset":128}),
        );
        let mut input = std::io::Cursor::new(raw.as_bytes());
        let mut line = Vec::new();
        let current = next_batch_request(&mut input, &mut line).unwrap().unwrap();
        assert_eq!(current.query, "Current");
        assert_eq!(current.generation, None);
        assert_eq!(current.mode, QueryMode::Locate);
        assert_eq!(current.offset, 0);
        assert!(!current.paged_source);
        let retained = next_batch_request(&mut input, &mut line).unwrap().unwrap();
        assert_eq!(retained.query, "Retained");
        assert_eq!(retained.generation, Some(generation));
        assert_eq!(retained.path_prefix.as_deref(), Some("src/retained.rs"));
        assert_eq!(retained.offset, 128);
        assert!(retained.paged_source);
        assert!(next_batch_request(&mut input, &mut line).unwrap().is_none());
    }

    #[test]
    fn invalid_later_requests_cannot_qualify_the_completed_prefix() {
        for invalid in [
            serde_json::json!({"query":""}),
            serde_json::json!({"query":"x".repeat(129)}),
            serde_json::json!({"query":"Second","paged_source":true}),
            serde_json::json!({"query":"Second","generation":"invalid"}),
            serde_json::json!({"query":"Second","unknown":true}),
            serde_json::json!({"query":"Second","mode":"unknown"}),
            serde_json::json!({"query":"Second","offset":-1}),
            serde_json::json!({"query":"Second","mode":"text","path_prefix":"src/sample.rs","paged_source":true}),
        ] {
            let raw = format!("{{\"query\":\"First\"}}\n{invalid}\n");
            let mut output = Vec::new();
            let mut attempted = Vec::new();
            assert!(
                run_batch(
                    &mut std::io::Cursor::new(raw.as_bytes()),
                    &mut output,
                    |request| {
                        attempted.push(request.query.clone());
                        Ok(serde_json::Value::Null)
                    },
                )
                .is_err()
            );
            assert_eq!(attempted, ["First"]);
            let rows = output_records(&output);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0]["index"], 0);
            assert_eq!(rows[1]["completed_requests"], 1);
            assert_eq!(rows[1]["complete"], false);
        }
    }

    #[test]
    fn streams_large_batches_without_an_aggregate_request_or_byte_ceiling() {
        const REQUESTS: usize = 25_000;
        let mut input = Vec::new();
        for index in 0..REQUESTS {
            writeln!(
                input,
                "{}",
                serde_json::json!({"query":format!("key_{index}")})
            )
            .unwrap();
        }
        assert!(input.len() > super::MAX_BATCH_REQUEST_LINE_BYTES);
        let mut output = Vec::new();
        let mut attempted = 0;
        run_batch(&mut std::io::Cursor::new(input), &mut output, |request| {
            assert_eq!(request.query, format!("key_{attempted}"));
            attempted += 1;
            Ok(serde_json::Value::Null)
        })
        .unwrap();
        assert_eq!(attempted, REQUESTS);
        let rows = output_records(&output);
        assert_eq!(rows.len(), REQUESTS + 1);
        assert_eq!(rows[REQUESTS - 1]["index"], REQUESTS - 1);
        assert_eq!(rows[REQUESTS]["completed_requests"], REQUESTS);
        assert_eq!(rows[REQUESTS]["complete"], true);
    }

    #[test]
    fn empty_blank_array_and_invalid_utf8_inputs_fail_with_an_incomplete_footer() {
        for raw in [b"".as_slice(), b"\n", b"[]\n", b"\xff\n"] {
            let mut output = Vec::new();
            let mut attempted = 0;
            assert!(
                run_batch(&mut std::io::Cursor::new(raw), &mut output, |_| {
                    attempted += 1;
                    Ok(serde_json::Value::Null)
                })
                .is_err()
            );
            assert_eq!(attempted, 0);
            let rows = output_records(&output);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["completed_requests"], 0);
            assert_eq!(rows[0]["complete"], false);
        }
    }

    #[test]
    fn oversized_request_lines_are_rejected_before_query_execution() {
        let raw = vec![b' '; super::MAX_BATCH_REQUEST_LINE_BYTES * 2];
        let mut input = std::io::Cursor::new(raw);
        let mut line = Vec::new();
        assert!(next_batch_request(&mut input, &mut line).is_err());
        assert_eq!(line.len(), super::MAX_BATCH_REQUEST_LINE_BYTES + 1);
        assert_eq!(input.position(), u64::try_from(line.len()).unwrap());
    }

    #[test]
    fn query_failure_stops_the_stream_and_marks_only_flushed_results_completed() {
        let raw = b"{\"query\":\"First\"}\n{\"query\":\"Second\"}\n{\"query\":\"Third\"}\n";
        let mut output = Vec::new();
        let mut attempted = 0;
        let error = run_batch(&mut std::io::Cursor::new(raw), &mut output, |_| {
            attempted += 1;
            if attempted == 2 {
                return Err("query failed".into());
            }
            Ok(serde_json::Value::Null)
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "query failed");
        assert_eq!(attempted, 2);
        let rows = output_records(&output);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["completed_requests"], 1);
        assert_eq!(rows[1]["complete"], false);
    }

    #[test]
    fn input_failure_cannot_turn_a_completed_prefix_into_a_full_batch() {
        let mut input = FailAfterPrefix {
            prefix: std::io::Cursor::new(b"{\"query\":\"First\"}\n".to_vec()),
        };
        let mut output = Vec::new();
        let error =
            run_batch(&mut input, &mut output, |_| Ok(serde_json::Value::Null)).unwrap_err();
        assert_eq!(error.to_string(), "input read failed");
        let rows = output_records(&output);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["completed_requests"], 1);
        assert_eq!(rows[1]["complete"], false);
    }

    #[test]
    fn failed_entry_flush_does_not_count_a_request_as_completed() {
        let mut output = FailFlush::default();
        let error = run_batch(
            &mut std::io::Cursor::new(b"{\"query\":\"First\"}\n"),
            &mut output,
            |_| Ok(serde_json::Value::Null),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "output flush failed");
        let rows = output_records(&output.bytes);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["completed_requests"], 0);
        assert_eq!(rows[1]["complete"], false);
    }

    #[test]
    fn text_queries_preserve_explicit_generations_filters_and_symbol_free_results() {
        let fixture = tempfile::tempdir().unwrap();
        let repository_root = std::fs::canonicalize(fixture.path()).unwrap();
        let source = repository_root.join("module.rs");
        std::fs::write(&source, "// retained_needle\npub fn anchor() {}\n").unwrap();
        std::fs::write(
            repository_root.join("other.rs"),
            "// retained_needle\npub fn other() {}\n",
        )
        .unwrap();
        let cancellation = rootlight_cancel::Cancellation::new();
        let mut service = rootlight_service::FirstSliceService::new(2).unwrap();
        let first = service
            .index_repository_with_mode(
                &repository_root,
                rootlight_service::FirstSliceIndexMode::Structural,
                &cancellation,
            )
            .unwrap();
        std::fs::write(&source, "// successor_needle\npub fn anchor() {}\n").unwrap();
        let second = service
            .index_repository_with_mode(
                &repository_root,
                rootlight_service::FirstSliceIndexMode::Structural,
                &cancellation,
            )
            .unwrap();
        assert_ne!(first.generation, second.generation);
        let mut request = QueryRequest {
            query: "retained_needle".to_owned(),
            mode: QueryMode::Text,
            offset: 0,
            path_prefix: Some("module.rs".to_owned()),
            generation: Some(first.generation),
            paged_source: false,
            find: None,
        };
        let result =
            query_existing(&service, first.repository, &request, false, &cancellation).unwrap();
        assert_eq!(result["schema"], "rootlight-reopen-text-query-diagnostic/1");
        assert_eq!(result["status"]["generation"], first.generation.to_string());
        let hits = result["locate"]["data"]["hits"].as_array().unwrap();
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|hit| hit["symbol"].is_null()));
        assert!(result["details"].as_array().unwrap().is_empty());
        request.generation = Some(second.generation);
        let absent =
            query_existing(&service, first.repository, &request, false, &cancellation).unwrap();
        assert!(
            absent["locate"]["data"]["hits"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        request.query = "anchor".to_owned();
        request.mode = QueryMode::Locate;
        let located =
            query_existing(&service, first.repository, &request, false, &cancellation).unwrap();
        assert_eq!(located["schema"], "rootlight-reopen-query-diagnostic/1");
        assert!(!located["details"].as_array().unwrap().is_empty());
        request.query = "absent_symbol".to_owned();
        assert_eq!(
            query_existing(&service, first.repository, &request, false, &cancellation)
                .unwrap_err()
                .to_string(),
            "query returned no symbols"
        );
        let empty =
            query_existing(&service, first.repository, &request, true, &cancellation).unwrap();
        assert_eq!(empty["schema"], "rootlight-reopen-query-diagnostic/1");
        assert!(
            empty["locate"]["data"]["hits"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(empty["details"].as_array().unwrap().is_empty());
    }

    #[test]
    fn batch_offsets_preserve_public_duplicate_member_pagination() {
        let fixture = tempfile::tempdir().unwrap();
        let repository_root = std::fs::canonicalize(fixture.path()).unwrap();
        let members = (0..130)
            .map(|index| serde_json::json!({"reason":index}))
            .collect::<Vec<_>>();
        std::fs::write(
            repository_root.join("members.json"),
            serde_json::to_vec(&serde_json::json!({"items":members})).unwrap(),
        )
        .unwrap();
        let cancellation = rootlight_cancel::Cancellation::new();
        let mut service = rootlight_service::FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository_with_mode(
                &repository_root,
                rootlight_service::FirstSliceIndexMode::Structural,
                &cancellation,
            )
            .unwrap();
        let mut request = QueryRequest {
            query: "reason".to_owned(),
            mode: QueryMode::Locate,
            offset: 0,
            path_prefix: Some("members.json".to_owned()),
            generation: Some(receipt.generation),
            paged_source: false,
            find: None,
        };
        let expected_symbols = service
            .inspect_generation(
                receipt.repository,
                Some(receipt.generation),
                &cancellation,
                |snapshot| {
                    snapshot
                        .document()
                        .entities
                        .iter()
                        .filter(|entity| {
                            entity.kind == rootlight_ir::EntityKind::Property
                                && entity.display_name == "reason"
                        })
                        .map(|entity| entity.id.to_string())
                        .collect::<std::collections::BTreeSet<_>>()
                },
            )
            .unwrap();
        assert_eq!(expected_symbols.len(), 130);
        let mut symbols = std::collections::BTreeSet::new();
        let mut page_sizes = Vec::new();
        loop {
            let result =
                query_existing(&service, receipt.repository, &request, true, &cancellation)
                    .unwrap();
            let located = &result["locate"]["data"];
            let hits = located["hits"].as_array().unwrap();
            assert!(!hits.is_empty());
            assert!(hits.len() <= 64);
            page_sizes.push(hits.len());
            for hit in hits {
                if let Some(symbol) = hit["symbol"].as_str() {
                    assert!(expected_symbols.contains(symbol));
                    assert!(symbols.insert(symbol.to_owned()));
                } else {
                    assert_eq!(hit["path"], "members.json");
                    assert!(!hit["source"].is_null());
                }
            }
            let Some(next) = located["next_page_offset"].as_u64() else {
                assert!(located["next_page_offset"].is_null());
                break;
            };
            let next = usize::try_from(next).unwrap();
            assert_eq!(next, request.offset + hits.len());
            request.offset = next;
        }
        assert_eq!(page_sizes[..2], [64, 64]);
        assert_eq!(symbols, expected_symbols);
    }

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
