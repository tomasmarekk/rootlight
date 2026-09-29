//! Audits fresh or resumed durable indexing through public service and source APIs.
//! Explicit configuration and default detection are separate invocations; counts
//! describe recorded evidence, not an independent semantic completeness oracle.
//! Resume measures a reopened service; warm sequences keep one service alive and
//! wait for external source edits between newline-delimited reports.
//! Gap matches identify exact-span occurrences, not the semantic owner of a gap.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_ir::{
    FileRecord, NormalizedIrDocument, OccurrenceTarget, RelationEndpoint, RelationPredicate,
    SourceMappingKind, SourceRef, SourceSpan,
};
use rootlight_runtime::RuntimePaths;
use rootlight_service::{
    FirstSliceBudget, FirstSliceIndexMode, FirstSliceIndexProgress, FirstSliceService,
    FirstSliceStoragePolicy,
};
use rootlight_source::{SourceEncoding, SourceReadOptions};
use serde_json::json;

#[derive(Clone, Copy)]
enum StateMode {
    Fresh,
    Resume,
}

#[derive(Clone, Copy)]
enum ServiceLifetime {
    Fresh,
    Reopened,
    Warm,
}

struct DiagnosticSession {
    service: FirstSliceService,
    repository: PathBuf,
    config: Option<String>,
    open_wall_micros: u128,
    lifetime: ServiceLifetime,
}

fn deadline() -> Result<Cancellation, Box<dyn Error>> {
    let end = Instant::now()
        .checked_add(Duration::from_secs(3600))
        .ok_or("diagnostic deadline is not representable")?;
    Ok(Cancellation::with_deadline(end))
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(maximum.checked_add(1).ok_or("read bound overflow")?)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > maximum {
        return Err("diagnostic input exceeds its read bound".into());
    }
    Ok(bytes)
}

fn read_live_file(repository: &Path, file: &FileRecord) -> Result<Vec<u8>, Box<dyn Error>> {
    let relative = rootlight_vfs::RelativePath::parse(Path::new(&file.path))?;
    let path = fs::canonicalize(repository.join(relative.as_str()))?;
    if !path.starts_with(repository) {
        return Err("source escaped repository".into());
    }
    let live = read_bounded(&path, file.byte_length)?;
    if u64::try_from(live.len())? != file.byte_length
        || rootlight_ids::content_hash(&live) != file.content_hash
    {
        return Err("live source identity differs from the indexed file".into());
    }
    Ok(live)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let (mode, sequence, args) = match args.split_first() {
        Some((flag, tail)) if flag == "--resume" => (StateMode::Resume, false, tail),
        Some((flag, tail)) if flag == "--warm-sequence" => (StateMode::Fresh, true, tail),
        _ => (StateMode::Fresh, false, args.as_slice()),
    };
    if !(2..=3).contains(&args.len()) {
        return Err(
            "usage: index_coverage [--resume | --warm-sequence] REPOSITORY STATE_DIRECTORY [CONFIG_FILE]".into(),
        );
    }
    let repository = PathBuf::from(&args[0]);
    let state = PathBuf::from(&args[1]);
    let config = args
        .get(2)
        .map(|path| -> Result<String, Box<dyn Error>> {
            Ok(String::from_utf8(read_bounded(
                Path::new(path),
                u64::try_from(rootlight_config::MAX_CONFIG_LAYER_BYTES)?,
            )?)?)
        })
        .transpose()?;
    if sequence {
        let mut session = DiagnosticSession::open(&repository, &state, config.as_deref(), mode)?;
        run_sequence(
            &mut session,
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        )?;
    } else {
        let report = inspect(&repository, &state, config.as_deref(), mode)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

// The harness owns source edits. Flushing a full report is the handoff point;
// waiting for its next command is outside every recorded index duration.
fn run_sequence(
    session: &mut DiagnosticSession,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<(), Box<dyn Error>> {
    loop {
        serde_json::to_writer(&mut *output, &session.inspect()?)?;
        output.write_all(b"\n")?;
        output.flush()?;
        let mut command = Vec::new();
        (&mut *input).take(16).read_until(b'\n', &mut command)?;
        match command.as_slice() {
            b"index\n" | b"index\r\n" => {}
            b"quit\n" | b"quit\r\n" | b"" => return Ok(()),
            _ => return Err("expected index or quit followed by a newline".into()),
        }
    }
}

fn inspect(
    repository: &Path,
    state: &Path,
    config: Option<&str>,
    mode: StateMode,
) -> Result<serde_json::Value, Box<dyn Error>> {
    DiagnosticSession::open(repository, state, config, mode)?.inspect()
}

impl DiagnosticSession {
    fn open(
        repository: &Path,
        state: &Path,
        config: Option<&str>,
        mode: StateMode,
    ) -> Result<Self, Box<dyn Error>> {
        let repository = fs::canonicalize(repository)?;
        let policy = FirstSliceStoragePolicy::resolve_user_config(config, 2)?;
        match mode {
            // A previous generation must not masquerade as a cold run.
            StateMode::Fresh => fs::create_dir(state)?,
            StateMode::Resume if !state.join("state").is_dir() => {
                return Err("resume requires an existing durable state directory".into());
            }
            StateMode::Resume => {}
        }
        let paths = RuntimePaths::new(state.join("state"), state.join("runtime"))?;
        paths.prepare_owner()?;
        let opened = Instant::now();
        let service =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()?)?;
        let open_wall_micros = opened.elapsed().as_micros();
        Ok(Self {
            service,
            repository,
            config: config.map(str::to_owned),
            open_wall_micros,
            lifetime: match mode {
                StateMode::Fresh => ServiceLifetime::Fresh,
                StateMode::Resume => ServiceLifetime::Reopened,
            },
        })
    }

    fn inspect(&mut self) -> Result<serde_json::Value, Box<dyn Error>> {
        let Self {
            service,
            repository,
            config,
            open_wall_micros,
            lifetime,
        } = self;
        let previous_generation = match lifetime {
            ServiceLifetime::Fresh => None,
            ServiceLifetime::Reopened | ServiceLifetime::Warm => {
                let registered = service
                    .registered_repository_for_root(repository, &deadline()?)?
                    .ok_or("resume repository is not registered in this state")?;
                Some(
                    service
                        .repository_status(registered, None)?
                        .resolved_generation,
                )
            }
        };
        let (cold, cold_timing) = index_with_timing(service, repository)?;
        let (no_op, no_op_timing) = index_with_timing(service, repository)?;
        if cold != no_op {
            return Err("unchanged repository produced a different receipt".into());
        }
        let status = service.repository_status(cold.repository, Some(cold.generation))?;
        let (rows, configuration_hash) = service.inspect_generation(
            cold.repository,
            Some(cold.generation),
            &deadline()?,
            |snapshot| -> Result<_, Box<dyn Error>> {
                Ok((
                    inspect_files(snapshot, service, repository, cold.generation)?,
                    snapshot.metadata().configuration_hash(),
                ))
            },
        )??;
        let mut report = json!({
            "schema":"rootlight-index-coverage-diagnostic/4", "explicit_configuration":config.is_some(),
            "configuration_input_hash":rootlight_ids::content_hash(config.as_deref().unwrap_or("").as_bytes()).to_string(),
            "configuration_hash":configuration_hash,
            "open_wall_micros":open_wall_micros,
            "process_id":std::process::id(),
            "no_op_same_generation":true,"no_op_wall_micros":no_op_timing["wall_micros"],"no_op_timing":no_op_timing,
            "generation_inspection_method":"local_identity_verified_snapshot",
            "shared_bundle_roundtrip":false,
            "coverage":status.coverage.iter().map(|entry| json!({"language":entry.language,"indexed_files":entry.indexed_files,
                "discovered_files":entry.discovered_files,"tier":entry.tier,"status":entry.status})).collect::<Vec<_>>(),
            "files":rows,"full_semantic_completeness_proven":false,
            "installed_mcp_qualified":false
        });
        match lifetime {
            ServiceLifetime::Fresh => {
                report["service_lifetime"] = json!("fresh");
                report["cold"] = serde_json::to_value(cold)?;
                report["cold_wall_micros"] = cold_timing["wall_micros"].clone();
                report["cold_timing"] = cold_timing;
            }
            ServiceLifetime::Reopened | ServiceLifetime::Warm => {
                report["schema"] = json!("rootlight-index-update-diagnostic/1");
                report["service_lifetime"] = json!("reopened");
                if matches!(lifetime, ServiceLifetime::Warm) {
                    report["schema"] = json!("rootlight-index-warm-update-diagnostic/1");
                    report["service_lifetime"] = json!("warm");
                    report["open_wall_micros"] = serde_json::Value::Null;
                }
                report["previous_generation"] = serde_json::to_value(previous_generation)?;
                report["update"] = serde_json::to_value(cold)?;
                report["update_wall_micros"] = cold_timing["wall_micros"].clone();
                report["update_timing"] = cold_timing;
            }
        }
        *lifetime = ServiceLifetime::Warm;
        Ok(report)
    }
}

fn index_with_timing(
    service: &mut FirstSliceService,
    repository: &Path,
) -> Result<(rootlight_service::FirstSliceIndexReceipt, serde_json::Value), Box<dyn Error>> {
    let cancellation = deadline()?;
    let started = Instant::now();
    let mut timeline = Vec::new();
    let prepared = service.prepare_repository_with_mode_and_progress(
        repository,
        FirstSliceIndexMode::Structural,
        &cancellation,
        |progress: FirstSliceIndexProgress| {
            timeline.push((Instant::now(), progress));
        },
    )?;
    let prepared_at = Instant::now();
    let commit = service.publish_prepared_with_metrics(prepared, &cancellation)?;
    let published_at = Instant::now();
    let mut previous_event = started;
    let mut previous_completed = started;
    let mut completed = 0;
    let mut stages = Vec::new();
    let mut events = Vec::new();
    for (observed_at, progress) in timeline {
        events.push(json!({
            "stage":format!("{:?}", progress.stage),
            "completed":progress.completed,
            "since_start_micros":observed_at.duration_since(started).as_micros(),
            "since_previous_event_micros":observed_at.duration_since(previous_event).as_micros(),
            "files_examined":progress.files_examined,
            "bytes_examined":progress.bytes_examined,
            "written_bytes":progress.written_bytes,
        }));
        previous_event = observed_at;
        if progress.completed > completed {
            stages.push(json!({
                "stage":format!("{:?}", progress.stage),
                "completed":progress.completed,
                "wall_micros":observed_at.duration_since(previous_completed).as_micros(),
                "files_examined":progress.files_examined,
                "bytes_examined":progress.bytes_examined,
                "written_bytes":progress.written_bytes,
            }));
            previous_completed = observed_at;
            completed = progress.completed;
        }
    }
    let evidence = commit.evidence();
    Ok((
        commit.receipt().clone(),
        json!({
            "wall_micros":published_at.duration_since(started).as_micros(),
            "prepare_micros":prepared_at.duration_since(started).as_micros(),
            "publish_micros":published_at.duration_since(prepared_at).as_micros(),
            "after_last_progress_micros":prepared_at.duration_since(previous_completed).as_micros(),
            "completed_stages":stages,
            "progress_events":events,
            "operation":{
                "strategy":format!("{:?}", evidence.strategy),
                "fallback_reason":evidence.fallback_reason.as_ref().map(|reason| format!("{reason:?}")),
                "invalidated_units":evidence.invalidated_units,"changed_inputs":evidence.changed_inputs,
                "changed_files":evidence.changed_files,"reused_files":evidence.reused_files,
                "rebuilt_files":evidence.rebuilt_files,"reused_facts":evidence.reused_facts,
                "rebuilt_facts":evidence.rebuilt_facts,"referenced_bytes":evidence.referenced_bytes,
                "newly_written_bytes":evidence.newly_written_bytes,"reserved_memory_bytes":evidence.reserved_memory_bytes,
                "owned_memory_bytes":evidence.owned_memory_bytes,"retained_durable_bytes":evidence.retained_durable_bytes,
                "planned_fact_work":evidence.planned_fact_work,"normalized_fact_work":evidence.normalized_fact_work,
            },
        }),
    ))
}

fn inspect_files(
    snapshot: &rootlight_storage::GenerationSnapshot,
    service: &FirstSliceService,
    repository: &Path,
    generation: rootlight_ids::GenerationId,
) -> Result<Vec<serde_json::Value>, Box<dyn Error>> {
    let document = snapshot.document();
    let mut origins = BTreeMap::new();
    for mapping in &document.source_mappings {
        if mapping.kind == SourceMappingKind::ExtractedFromContainer
            && origins
                .insert(mapping.from.span().file(), &mapping.to)
                .is_some()
        {
            return Err("extracted source has multiple container origins".into());
        }
    }
    let mut supplemental_calls = supplemental_call_evidence(document)?;
    let mut verified_containers = BTreeSet::new();
    let mut rows = Vec::new();
    for file in document.files.iter().chain(
        snapshot
            .source_files()
            .entries()
            .iter()
            .map(|entry| entry.file()),
    ) {
        let source = file
            .evidence
            .source
            .clone()
            .ok_or("file has no generation-bound source")?;
        if source.generation() != generation {
            return Err("file source belongs to a different generation".into());
        }
        let origin = origins.get(&file.id).copied();
        if let Some(container_ref) = origin {
            let container = snapshot
                .find_file(container_ref.span().file())
                .ok_or("extracted source has no retained container")?;
            if container_ref.repository() != file.repository
                || container_ref.generation() != generation
                || container_ref.span().start_byte() != 0
                || container_ref.span().end_byte() != container.byte_length
                || container_ref.content_hash() != container.content_hash
            {
                return Err("extracted source has an invalid container reference".into());
            }
            if verified_containers.insert(container.id) {
                read_live_file(repository, container)?;
            }
        }
        let live = origin
            .is_none()
            .then(|| read_live_file(repository, file))
            .transpose()?;
        let text = live
            .as_deref()
            .filter(|_| file.encoding == "utf-8")
            .and_then(|bytes| std::str::from_utf8(bytes).ok());
        let encoding = if text.is_some() {
            SourceEncoding::Utf8
        } else {
            SourceEncoding::Bytes
        };
        let file_length = usize::try_from(file.byte_length)?;
        let mut returned_hasher = blake3::Hasher::new();
        let mut start = 0_usize;
        loop {
            let end = text.map_or_else(
                || start.saturating_add(16 * 1024).min(file_length),
                |text| chunk_end(text, start),
            );
            let part = SourceRef::new(
                source.repository(),
                source.generation(),
                SourceSpan::new(file.id, u64::try_from(start)?, u64::try_from(end)?)?,
                source.content_hash(),
                None,
            );
            let read = service.source_read_with_options_and_budget(
                generation,
                vec![part.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(encoding),
                FirstSliceBudget::default(),
                &deadline()?,
            )?;
            if read.data.generation != generation || read.data.chunks.len() != 1 {
                return Err(format!(
                    "source retrieval returned an unexpected generation or chunk count: {}",
                    file.path
                )
                .into());
            }
            let chunk = &read.data.chunks[0];
            if chunk.reference != part
                || chunk.extracted_from.as_ref() != origin
                || chunk.path != file.path
                || chunk.content_hash != file.content_hash
                || chunk.language != file.language
                || chunk.start_byte != u64::try_from(start)?
                || chunk.end_byte != u64::try_from(end)?
                || live
                    .as_ref()
                    .is_some_and(|bytes| Some(chunk.bytes.as_slice()) != bytes.get(start..end))
            {
                return Err(format!("source retrieval differs from indexed evidence: {} at {start}..{end}, returned {}..{} ({} bytes)",
                    file.path, chunk.start_byte, chunk.end_byte, chunk.bytes.len()).into());
            }
            returned_hasher.update(&chunk.bytes);
            if end == file_length {
                break;
            }
            start = end;
        }
        if rootlight_ids::ContentHash::from_bytes(*returned_hasher.finalize().as_bytes())
            != file.content_hash
        {
            return Err(format!(
                "retrieved source hash differs from indexed file: {}",
                file.path
            )
            .into());
        }
        let mut occurrences = BTreeMap::<String, usize>::new();
        let mut sites_by_span = BTreeMap::<_, Vec<_>>::new();
        for site in &document.occurrences {
            if site.source.span().file() != file.id {
                continue;
            }
            let resolution = match site.target {
                OccurrenceTarget::Resolved { .. } => "resolved",
                OccurrenceTarget::Candidates { .. } => "candidates",
                OccurrenceTarget::Unresolved { .. } => "unresolved",
            };
            *occurrences
                .entry(format!("{}:{resolution}", site.syntax_kind))
                .or_default() += 1;
            sites_by_span
                .entry(site.source.span())
                .or_default()
                .push(site);
        }
        for sites in sites_by_span.values_mut() {
            sites.sort_by_key(|site| (&site.syntax_kind, site.id));
        }
        let gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.source.span().file() == file.id)
            .map(|gap| {
                let mut value = serde_json::to_value(gap)?;
                // A callee and its application can share a span but disagree on
                // resolution. Retain all matches instead of guessing gap ownership.
                let matching: Vec<_> = sites_by_span
                    .get(&gap.source.span())
                    .into_iter()
                    .flatten()
                    .filter(|site| {
                        site.source.repository() == gap.source.repository()
                            && site.source.generation() == gap.source.generation()
                            && site.source.content_hash() == gap.source.content_hash()
                    })
                    .map(|site| {
                        json!({
                            "id":site.id, "syntax_kind":site.syntax_kind,
                            "role":site.role, "target":site.target,
                        })
                    })
                    .collect();
                value["matching_occurrences"] = json!(matching);
                Ok(value)
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?;
        let container_path = origin.and_then(|reference| {
            snapshot
                .find_file(reference.span().file())
                .map(|container| container.path.as_str())
        });
        rows.push(json!({
            "file":file,
            "source_exact":live.is_some(),
            "source_read_exact":true,
            "extracted_from":origin,
            "container_path":container_path,
            "container_live_exact":origin.map(|_| true),
            "extraction_independently_verified":origin.map(|_| false),
            "occurrences":occurrences,
            "gaps":gaps,
            "supplemental_calls":supplemental_calls.remove(&file.id).unwrap_or_default()
        }));
    }
    if rows.len() != snapshot.file_count() {
        return Err("inspection omitted a retained file record".into());
    }
    Ok(rows)
}

fn supplemental_call_evidence(
    document: &NormalizedIrDocument,
) -> Result<BTreeMap<rootlight_ids::FileId, Vec<serde_json::Value>>, Box<dyn Error>> {
    let occurrences: BTreeMap<_, _> = document
        .occurrences
        .iter()
        .map(|site| (site.id, site))
        .collect();
    let entities: BTreeMap<_, _> = document
        .entities
        .iter()
        .map(|entity| (entity.id, entity))
        .collect();
    let provenance: BTreeMap<_, _> = document
        .provenance
        .iter()
        .map(|proof| (proof.id, proof))
        .collect();
    let mut result: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for relation in &document.relations {
        if relation.predicate != RelationPredicate::Calls {
            continue;
        }
        let (RelationEndpoint::Occurrence(id), RelationEndpoint::Entity(target)) =
            (relation.subject, relation.object)
        else {
            continue;
        };
        let site = occurrences.get(&id).ok_or("call has no occurrence")?;
        if site.target == (OccurrenceTarget::Resolved { symbol: target }) {
            continue;
        }
        // Additional effects can call a different symbol than the occurrence's
        // lexical target. Counts alone cannot audit either identity or its proof.
        result.entry(site.file).or_default().push(json!({
            "relation":relation,
            "occurrence":site,
            "lexical_target":match site.target {
                OccurrenceTarget::Resolved { symbol } => entities.get(&symbol).copied(),
                _ => None,
            },
            "target":entities.get(&target).ok_or("call has no target entity")?,
            "provenance":provenance.get(&relation.provenance).ok_or("call has no provenance")?,
        }));
    }
    Ok(result)
}

fn chunk_end(text: &str, start: usize) -> usize {
    let mut end = start.saturating_add(16 * 1024).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplemental_calls_preserve_distinct_targets_and_conditions_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(
            repository.join("project.vbp"),
            "Type=Exe\nName=\"Project\"\nModule=Client; Client.bas\nClass=Worker; Worker.cls\n",
        )
        .unwrap();
        fs::write(repository.join("Client.bas"), "Attribute VB_Name = \"Client\"\nSub Run()\nDim item As New Worker\nitem.Touch\nEnd Sub\n").unwrap();
        fs::write(repository.join("Worker.cls"), "VERSION 1.0 CLASS\nAttribute VB_Name = \"Worker\"\nPrivate Sub Class_Initialize()\nEnd Sub\nPublic Sub Touch()\nEnd Sub\n").unwrap();
        let state = directory.path().join("state");
        let report = inspect(&repository, &state, None, StateMode::Fresh).unwrap();
        let calls: Vec<_> = report["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row["supplemental_calls"].as_array().unwrap())
            .collect();
        assert_eq!(calls.len(), 1);
        let call = calls[0];
        assert_eq!(call["target"]["canonical_name"], "Class_Initialize");
        assert_eq!(call["lexical_target"]["canonical_name"], "item");
        assert_eq!(
            call["lexical_target"]["id"],
            call["occurrence"]["target"]["symbol"]
        );
        assert_eq!(call["occurrence"]["target"]["kind"], "resolved");
        assert_ne!(call["occurrence"]["target"]["symbol"], call["target"]["id"]);
        assert_eq!(call["relation"]["predicate"], "calls");
        assert_eq!(
            call["provenance"]["rule"],
            "classic-automatic-initialization-v1.when_nothing"
        );
        assert_eq!(call["relation"]["provenance"], call["provenance"]["id"]);
        let restored = inspect(&repository, &state, None, StateMode::Resume).unwrap();
        assert_eq!(report["files"], restored["files"]);
    }

    #[test]
    fn gap_evidence_distinguishes_callees_applications_and_shared_spans() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        let source = "Attribute VB_Name = \"Module\"\nSub Visit(ByVal required As Long)\nEnd Sub\nSub Run()\nDim result As Long\nresult = Missing(1)\nOther\nVisit\nEnd Sub\n";
        fs::write(repository.join("Module.bas"), source).unwrap();
        let report = inspect(
            &repository,
            &directory.path().join("state"),
            None,
            StateMode::Fresh,
        )
        .unwrap();
        let row = &report["files"][0];
        for (text, kinds) in [
            ("Missing", vec!["vba.invoked_get_identifier.reference"]),
            ("Missing(1)", vec!["vba.application.reference"]),
            (
                "Other",
                vec![
                    "vba.application.reference",
                    "vba.invoked_identifier.reference",
                ],
            ),
        ] {
            let start = u64::try_from(source.find(text).unwrap()).unwrap();
            let end = start + u64::try_from(text.len()).unwrap();
            let gaps: Vec<_> = row["gaps"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|gap| {
                    gap["source"]["span"]["start_byte"] == start
                        && gap["source"]["span"]["end_byte"] == end
                })
                .collect();
            assert!(!gaps.is_empty(), "missing gap: {text}: {row}");
            for gap in gaps {
                let matches = gap["matching_occurrences"]
                    .as_array()
                    .expect("exact-span occurrence evidence");
                let observed: Vec<_> = matches
                    .iter()
                    .map(|site| site["syntax_kind"].as_str().unwrap())
                    .collect();
                assert_eq!(observed, kinds, "{text}");
                assert!(
                    matches
                        .iter()
                        .all(|site| site["target"]["kind"] == "unresolved")
                );
            }
        }
        let start = u64::try_from(source.rfind("Visit").unwrap()).unwrap();
        let gap = row["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|gap| {
                gap["source"]["span"]["start_byte"] == start
                    && gap["source"]["span"]["end_byte"] == start + 5
            })
            .unwrap();
        let matching = gap["matching_occurrences"].as_array().unwrap();
        assert_eq!(matching.len(), 2);
        assert_eq!(matching[0]["syntax_kind"], "vba.application.reference");
        assert_eq!(matching[0]["target"]["kind"], "unresolved");
        assert_eq!(
            matching[1]["syntax_kind"],
            "vba.invoked_identifier.reference"
        );
        assert_eq!(matching[1]["target"]["kind"], "resolved");
        assert_eq!(matching[1]["role"], "call_site");
        assert_eq!(row["source_read_exact"], true);
        assert_eq!(report["no_op_same_generation"], true);
        let restored = inspect(
            &repository,
            &directory.path().join("state"),
            None,
            StateMode::Resume,
        )
        .unwrap();
        assert_eq!(restored["files"], report["files"]);
    }

    #[test]
    fn warm_reports_keep_one_session_through_edits_and_restoration() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        let file = repository.join("value.rs");
        let original = "pub fn value() -> i32 { 1 }\n";
        fs::write(&file, original).unwrap();
        fs::write(repository.join("other.rs"), "pub fn other() -> i32 { 3 }\n").unwrap();
        let state = directory.path().join("state");
        let mut session =
            DiagnosticSession::open(&repository, &state, None, StateMode::Fresh).unwrap();
        let cold = session.inspect().unwrap();
        let mut previous = cold["cold"]["generation"].clone();
        for source in ["pub fn value() -> i32 { 2 }\n", original] {
            fs::write(&file, source).unwrap();
            let report = session.inspect().unwrap();
            assert_eq!(report["schema"], "rootlight-index-warm-update-diagnostic/1");
            assert_eq!(report["service_lifetime"], "warm");
            assert_eq!(report["process_id"], cold["process_id"]);
            assert!(report["open_wall_micros"].is_null());
            assert!(report.get("cold").is_none());
            assert_eq!(report["previous_generation"], previous);
            assert_eq!(report["update"]["parent"], previous);
            assert_ne!(report["update"]["generation"], previous);
            previous = report["update"]["generation"].clone();
            let work = &report["update_timing"]["operation"];
            assert_eq!(work["rebuilt_files"], 1);
            assert_eq!(work["reused_files"], 1);
            assert!(work["reused_facts"].as_u64().unwrap() > 0);
            assert_eq!(
                work["rebuilt_facts"].as_u64().unwrap() + work["reused_facts"].as_u64().unwrap(),
                cold["cold_timing"]["operation"]["rebuilt_facts"]
                    .as_u64()
                    .unwrap()
            );
            assert_eq!(report["no_op_timing"]["operation"]["rebuilt_files"], 0);
            assert_eq!(report["no_op_timing"]["operation"]["rebuilt_facts"], 0);
            assert!(
                report["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["source_read_exact"] == true)
            );
        }
        assert_eq!(fs::read(&file).unwrap(), original.as_bytes());
    }

    #[test]
    fn sequence_protocol_emits_complete_reports_and_bounds_commands() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("value.rs"), "pub fn value() {}\n").unwrap();
        for (ordinal, command, count, valid) in [
            (0, "index\nindex\r\nquit\n", 3, true),
            (1, "", 1, true),
            (2, "unknown\n", 1, false),
            (3, "index", 1, false),
            (4, "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n", 1, false),
        ] {
            let mut session = DiagnosticSession::open(
                &repository,
                &directory.path().join(format!("state-{ordinal}")),
                None,
                StateMode::Fresh,
            )
            .unwrap();
            let mut input = std::io::Cursor::new(command.as_bytes());
            let mut output = Vec::new();
            assert_eq!(
                run_sequence(&mut session, &mut input, &mut output).is_ok(),
                valid
            );
            let reports: Vec<serde_json::Value> = output
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice(line).unwrap())
                .collect();
            assert_eq!(reports.len(), count);
            assert_eq!(reports[0]["service_lifetime"], "fresh");
            for report in reports.iter().skip(1) {
                assert_eq!(report["service_lifetime"], "warm");
                assert_eq!(
                    report["update"]["generation"],
                    reports[0]["cold"]["generation"]
                );
                assert_eq!(
                    report["update_timing"]["operation"]["strategy"],
                    "RetainedGeneration"
                );
            }
            if ordinal == 4 {
                assert_eq!(input.position(), 16);
            }
        }
    }

    #[test]
    fn resumed_diagnostic_tracks_body_edits_and_source_restoration() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let repository = root.join("repository");
        let state = root.join("state");
        fs::create_dir(&repository).unwrap();
        let original = "pub fn value() -> i32 { 1 }\n";
        let modified = "pub fn value() -> i32 { 2 }\n";
        let file = repository.join("value.rs");
        fs::write(&file, original).unwrap();
        fs::write(repository.join("other.rs"), "pub fn other() -> i32 { 3 }\n").unwrap();
        assert!(inspect(&repository, &state, None, StateMode::Resume).is_err());
        assert!(!state.exists());
        let cold = inspect(&repository, &state, None, StateMode::Fresh).unwrap();
        assert_eq!(cold["cold_timing"]["operation"]["strategy"], "Initial");
        assert_eq!(cold["cold_timing"]["operation"]["rebuilt_files"], 2);
        let retained = inspect(&repository, &state, None, StateMode::Resume).unwrap();
        assert_eq!(retained["update"]["generation"], cold["cold"]["generation"]);
        assert_eq!(
            retained["update_timing"]["operation"]["strategy"],
            "RetainedGeneration"
        );
        fs::write(&file, modified).unwrap();
        let changed = inspect(&repository, &state, None, StateMode::Resume).unwrap();
        assert_eq!(changed["schema"], "rootlight-index-update-diagnostic/1");
        assert_eq!(changed["service_lifetime"], "reopened");
        assert!(changed.get("cold").is_none());
        assert_eq!(changed["previous_generation"], cold["cold"]["generation"]);
        assert_eq!(changed["update"]["parent"], cold["cold"]["generation"]);
        assert_ne!(changed["update"]["generation"], cold["cold"]["generation"]);
        let work = &changed["update_timing"]["operation"];
        assert_eq!(work["changed_files"], 1);
        // Reopening does not restore the optional in-memory parser cache.
        // Account for actual work without requiring that cache to survive.
        assert_eq!(
            work["rebuilt_files"].as_u64().unwrap() + work["reused_files"].as_u64().unwrap(),
            2
        );
        assert_eq!(
            work["rebuilt_facts"].as_u64().unwrap() + work["reused_facts"].as_u64().unwrap(),
            cold["cold_timing"]["operation"]["rebuilt_facts"]
                .as_u64()
                .unwrap()
        );
        assert!(!work["normalized_fact_work"].as_array().unwrap().is_empty());
        assert_eq!(changed["no_op_timing"]["operation"]["changed_files"], 0);
        assert_eq!(changed["no_op_timing"]["operation"]["rebuilt_facts"], 0);
        assert_eq!(fs::read(&file).unwrap(), modified.as_bytes());
        fs::write(&file, original).unwrap();
        let restored = inspect(&repository, &state, None, StateMode::Resume).unwrap();
        assert_eq!(
            restored["previous_generation"],
            changed["update"]["generation"]
        );
        for report in [&cold, &retained, &changed, &restored] {
            assert_eq!(report["no_op_same_generation"], true);
            assert!(report["open_wall_micros"].as_u64().is_some());
            assert!(
                report["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["source_read_exact"] == true)
            );
        }
        assert_eq!(fs::read(&file).unwrap(), original.as_bytes());
        let other = root.join("unregistered");
        fs::create_dir(&other).unwrap();
        assert!(inspect(&other, &state, None, StateMode::Resume).is_err());
    }

    #[test]
    fn timing_reports_actual_warm_parser_and_fact_reuse() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        let file = repository.join("value.rs");
        fs::write(&file, "pub fn value() -> i32 { 1 }\n").unwrap();
        fs::write(repository.join("other.rs"), "pub fn other() -> i32 { 3 }\n").unwrap();
        let paths = RuntimePaths::new(
            directory.path().join("state"),
            directory.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &deadline().unwrap()).unwrap();
        let (cold, _) = index_with_timing(&mut service, &repository).unwrap();
        fs::write(&file, "pub fn value() -> i32 { 2 }\n").unwrap();
        let (changed, timing) = index_with_timing(&mut service, &repository).unwrap();
        assert_eq!(changed.parent, Some(cold.generation));
        let work = &timing["operation"];
        assert_eq!(work["strategy"], "DependencyDirected");
        assert!(work["fallback_reason"].is_null());
        assert_eq!(work["changed_files"], 1);
        assert_eq!(work["rebuilt_files"], 1);
        assert_eq!(work["reused_files"], 1);
        assert!(work["reused_facts"].as_u64().unwrap() > 0);
        assert!(work["rebuilt_facts"].as_u64().unwrap() > 0);
        let (no_op, timing) = index_with_timing(&mut service, &repository).unwrap();
        assert_eq!(no_op, changed);
        assert_eq!(timing["operation"]["strategy"], "RetainedGeneration");
        assert_eq!(timing["operation"]["rebuilt_files"], 0);
        assert_eq!(timing["operation"]["rebuilt_facts"], 0);
    }

    #[test]
    fn bounded_reads_reject_oversized_and_missing_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        assert!(read_bounded(&path, 3).is_err());
        fs::write(&path, b"abcd").unwrap();
        assert!(read_bounded(&path, 3).is_err());
        assert_eq!(read_bounded(&path, 4).unwrap(), b"abcd");
        assert!(read_bounded(&path, u64::MAX).is_err());
        fs::write(&path, b"").unwrap();
        assert!(read_bounded(&path, 0).unwrap().is_empty());
    }

    #[test]
    fn source_chunks_preserve_utf8_boundaries_and_empty_files() {
        for tail in ["x", "é", "界", "🦀"] {
            let text = format!("{}{}\r\n", "x".repeat(16 * 1024 - 1), tail.repeat(10_000));
            let mut start = 0;
            let mut reconstructed = String::new();
            while start < text.len() {
                let end = chunk_end(&text, start);
                assert!(end > start && end - start <= 16 * 1024);
                reconstructed.push_str(text.get(start..end).unwrap());
                start = end;
            }
            assert_eq!(reconstructed, text);
        }
        assert_eq!(chunk_end("", 0), 0);
    }

    #[test]
    fn diagnostic_reads_multichunk_utf16_without_transcoding() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        for (name, little_endian) in [("little.txt", true), ("big.txt", false)] {
            let source = "λ𝄞\r\n".repeat(5_000);
            let bytes = std::iter::once(0xfeff_u16)
                .chain(source.encode_utf16())
                .flat_map(|unit| {
                    if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>();
            fs::write(repository.join(name), bytes).unwrap();
        }
        let report = inspect(&repository, &root.join("state"), None, StateMode::Fresh).unwrap();
        let files = report["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|file| file["source_exact"] == true));
        assert!(files.iter().all(|file| file["source_read_exact"] == true));
        assert!(files.iter().all(|file| file["extracted_from"].is_null()));
        assert!(
            files
                .iter()
                .all(|file| !file["gaps"].as_array().unwrap().is_empty())
        );
        assert_eq!(report["no_op_same_generation"], true);
        assert_eq!(report["full_semantic_completeness_proven"], false);
        for file in files {
            let expected = if file["file"]["path"] == "little.txt" {
                "utf-16le"
            } else {
                "utf-16be"
            };
            assert_eq!(file["file"]["encoding"], expected);
        }
    }

    #[test]
    fn diagnostic_checks_exact_sources_and_distinguishes_language_assignments() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("empty.txt"), b"").unwrap();
        fs::write(
            repository.join("notes.txt"),
            format!("{}🦀\r\n", "x".repeat(16 * 1024 - 1)),
        )
        .unwrap();
        fs::write(
            repository.join("module.bas"),
            b"Attribute VB_Name = \"Metrics\"\r\nSub Run()\r\nEnd Sub\r\n",
        )
        .unwrap();
        let configured = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n";
        for (mode, config) in [("default", None), ("configured", Some(configured))] {
            let state = root.join(mode);
            let report = inspect(&repository, &state, config, StateMode::Fresh).unwrap();
            assert_eq!(report["schema"], "rootlight-index-coverage-diagnostic/4");
            assert_eq!(report["explicit_configuration"], config.is_some());
            assert_eq!(report["no_op_same_generation"], true);
            assert!(report["cold_timing"]["prepare_micros"].as_u64().is_some());
            assert!(report["cold_timing"]["publish_micros"].as_u64().is_some());
            assert!(
                !report["cold_timing"]["completed_stages"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            assert!(
                report["no_op_timing"]["completed_stages"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                report["cold_wall_micros"],
                report["cold_timing"]["wall_micros"]
            );
            assert_eq!(
                report["no_op_wall_micros"],
                report["no_op_timing"]["wall_micros"]
            );
            assert_eq!(report["full_semantic_completeness_proven"], false);
            assert_eq!(
                report["generation_inspection_method"],
                "local_identity_verified_snapshot"
            );
            assert_eq!(report["shared_bundle_roundtrip"], false);
            let files = report["files"].as_array().unwrap();
            assert_eq!(files.len(), 3);
            assert!(files.iter().all(|file| file["source_exact"] == true));
            assert!(files.iter().all(|file| file["source_read_exact"] == true));
            let module = files
                .iter()
                .find(|file| file["file"]["path"] == "module.bas")
                .unwrap();
            assert_eq!(
                module["file"]["language"],
                if config.is_some() {
                    "vba"
                } else {
                    "visual-basic-classic"
                }
            );
            assert!(inspect(&repository, &state, config, StateMode::Fresh).is_err());
        }
    }
}
