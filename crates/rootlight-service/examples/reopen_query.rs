//! Queries an existing durable index through public service APIs.
//! This diagnostic avoids whole-generation transfer so large repositories can
//! prove individual source-bound query trajectories without changing limits.

use std::{
    collections::BTreeSet,
    error::Error,
    path::PathBuf,
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_runtime::RuntimePaths;
use rootlight_service::{
    FirstSliceBudget, FirstSliceService, LocateMode, RelationDirection, RelationFamily,
    SourceEncoding, SourceReadOptions,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(3..=4).contains(&args.len()) {
        return Err("usage: reopen_query STATE_DIRECTORY REPOSITORY QUERY [PATH_PREFIX]".into());
    }
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
    let mut details = Vec::new();
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
        let call_evidence_total = call_sources.clone().count();
        let mut call_evidence_reads = Vec::new();
        for reference in call_sources.take(16) {
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
            call_evidence_reads.push(evidence);
        }
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
