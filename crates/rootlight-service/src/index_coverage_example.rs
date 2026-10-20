//! Audits fresh or resumed durable indexing through public service and source APIs.
//! Explicit configuration and default detection are separate invocations; counts
//! describe recorded evidence, not an independent semantic completeness oracle.
//! Resume measures a reopened service; warm sequences keep one service alive and
//! wait for external source edits between newline-delimited reports.
//! Observations have no fixed wall-clock cutoff; failures retain their phase.
//! Gap matches identify exact-span occurrences, not the semantic owner of a gap.
//! Bounded gap text comes from verified source reads; omissions carry reasons.
//! Opt-in source accounting verifies every retained source without exporting
//! normalized binding witnesses or treating absent details as semantic proof.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    ffi::OsString,
    fs,
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use rootlight_cancel::Cancellation;
use rootlight_ir::{
    FileRecord, OccurrenceRecord, OccurrenceTarget, SkippedRegion, SourceMappingKind, SourceRef,
    SourceSpan,
};
use rootlight_runtime::RuntimePaths;
use rootlight_service::{
    FirstSliceBudget, FirstSliceIndexMode, FirstSliceIndexProgress, FirstSliceService,
    FirstSliceStoragePolicy, capture_first_slice_analysis_profile,
};
use rootlight_source::{SourceEncoding, SourceReadOptions};
use serde_json::json;

#[path = "index_coverage_example/gap_text.rs"]
mod gap_text;

#[path = "index_coverage_example/binding_proofs.rs"]
mod binding_proofs;

#[path = "index_coverage_example/gap_census.rs"]
mod gap_census;

#[cfg(test)]
#[path = "container_sources/fixture.rs"]
mod fixture;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StateMode {
    Fresh,
    Resume,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReportMode {
    Full,
    SourceAccounting,
}

struct Arguments<'a> {
    state_mode: StateMode,
    sequence: bool,
    report_mode: ReportMode,
    analysis_profile: bool,
    paths: &'a [OsString],
}

fn parse_arguments(args: &[OsString]) -> Result<Arguments<'_>, Box<dyn Error>> {
    let mut state_mode = StateMode::Fresh;
    let mut sequence = false;
    let mut report_mode = ReportMode::Full;
    let mut analysis_profile = false;
    let mut lifetime_selected = false;
    let mut flags = 0;
    for argument in args {
        if !argument.to_string_lossy().starts_with("--") {
            break;
        }
        match argument.to_str() {
            Some("--resume") | Some("--warm-sequence") => {
                if lifetime_selected {
                    return Err("duplicate or conflicting service lifetime flag".into());
                }
                lifetime_selected = true;
                if argument == "--resume" {
                    state_mode = StateMode::Resume;
                } else {
                    sequence = true;
                }
            }
            Some("--source-accounting") => {
                if report_mode == ReportMode::SourceAccounting {
                    return Err("duplicate source-accounting flag".into());
                }
                report_mode = ReportMode::SourceAccounting;
            }
            Some("--analysis-profile") => {
                if analysis_profile {
                    return Err("duplicate analysis-profile flag".into());
                }
                analysis_profile = true;
            }
            _ => return Err("unknown diagnostic flag".into()),
        }
        flags += 1;
    }
    let paths = args.get(flags..).ok_or("invalid diagnostic arguments")?;
    if !(2..=3).contains(&paths.len())
        || paths
            .iter()
            .any(|argument| argument.to_string_lossy().starts_with("--"))
    {
        return Err("usage: index_coverage [--resume | --warm-sequence] [--source-accounting] [--analysis-profile] REPOSITORY STATE_DIRECTORY [CONFIG_FILE]".into());
    }
    if sequence && report_mode == ReportMode::SourceAccounting {
        return Err("source-accounting does not support warm sequences".into());
    }
    Ok(Arguments {
        state_mode,
        sequence,
        report_mode,
        analysis_profile,
        paths,
    })
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
    report_mode: ReportMode,
    analysis_profile: bool,
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
    let Arguments {
        state_mode: mode,
        sequence,
        report_mode,
        analysis_profile,
        paths: args,
    } = parse_arguments(&args)?;
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
        session.analysis_profile = analysis_profile;
        run_sequence(
            &mut session,
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        )?;
    } else {
        let report = if analysis_profile {
            let mut session =
                DiagnosticSession::open(&repository, &state, config.as_deref(), mode)?;
            session.report_mode = report_mode;
            session.analysis_profile = true;
            session.inspect()?
        } else {
            match report_mode {
                ReportMode::Full => inspect(&repository, &state, config.as_deref(), mode)?,
                ReportMode::SourceAccounting => {
                    inspect_source_accounting(&repository, &state, config.as_deref(), mode)?
                }
            }
        };
        write_pretty_report(&report, &mut std::io::stdout().lock())?;
    }
    Ok(())
}

fn write_pretty_report(
    report: &serde_json::Value,
    output: &mut impl Write,
) -> Result<(), Box<dyn Error>> {
    // Pretty JSON emits small fragments; buffer them before line-buffered stdout.
    let mut output = std::io::BufWriter::new(output);
    serde_json::to_writer_pretty(&mut output, report)?;
    output.write_all(b"\n")?;
    output.flush()?;
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

fn inspect_source_accounting(
    repository: &Path,
    state: &Path,
    config: Option<&str>,
    mode: StateMode,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let mut session = DiagnosticSession::open(repository, state, config, mode)?;
    session.report_mode = ReportMode::SourceAccounting;
    session.inspect()
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
        let service = FirstSliceService::new_durable_with_policy(
            policy,
            paths.state_dir(),
            &Cancellation::new(),
        )?;
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
            report_mode: ReportMode::Full,
            analysis_profile: false,
        })
    }

    fn inspect(&mut self) -> Result<serde_json::Value, Box<dyn Error>> {
        let Self {
            service,
            repository,
            config,
            open_wall_micros,
            lifetime,
            report_mode,
            analysis_profile,
        } = self;
        if *report_mode == ReportMode::SourceAccounting && matches!(lifetime, ServiceLifetime::Warm)
        {
            return Err("source-accounting does not support warm sequences".into());
        }
        let previous_generation = match lifetime {
            ServiceLifetime::Fresh => None,
            ServiceLifetime::Reopened | ServiceLifetime::Warm => {
                let registered = service
                    .registered_repository_for_root(repository, &Cancellation::new())?
                    .ok_or("resume repository is not registered in this state")?;
                Some(
                    service
                        .repository_status(registered, None)?
                        .resolved_generation,
                )
            }
        };
        let (cold, cold_timing) =
            index_with_optional_analysis_profile(service, repository, *analysis_profile)
                .map_err(|error| format!("index operation failed: {error}"))?;
        let (no_op, no_op_timing) =
            index_with_optional_analysis_profile(service, repository, *analysis_profile)
                .map_err(|error| format!("no-op verification failed: {error}"))?;
        if cold != no_op {
            return Err("unchanged repository produced a different receipt".into());
        }
        let status = service.repository_status(cold.repository, Some(cold.generation))?;
        let (rows, configuration_hash, details) =
            service.inspect_generation(
                cold.repository,
                Some(cold.generation),
                &Cancellation::new(),
                |snapshot| -> Result<_, Box<dyn Error>> {
                    if *report_mode == ReportMode::SourceAccounting {
                        let (rows, accounting) = inspect_source_files(
                            snapshot,
                            service,
                            repository,
                            cold.generation,
                        )?;
                        return Ok((rows, snapshot.metadata().configuration_hash(), json!({
                            "report_scope":"retained_source_accounting",
                            "source_accounting":accounting,
                            "aggregate_source_recovery":inspect_aggregate_source_recovery(
                                snapshot.document(), snapshot.metadata(), snapshot.file_count()
                            )?,
                            "normalized_proof_exported":false,
                            "structural_coverage_qualified":false,
                            "semantic_resolution_qualified":false,
                            "gap_census":null,
                            "gap_census_scope":"not_exported_source_accounting",
                            "exported_provenance":null,
                            "exported_fact_identities":null,
                            "exported_fact_identity_scope":"not_exported_source_accounting",
                            "exported_fact_root_scope":"not_exported_source_accounting"
                        })));
                    }
                    let mut evidence = binding_proofs::select_report_evidence(snapshot.document())?;
                    let mut census = gap_census::GapCensus::new();
                    let rows = inspect_files(
                        snapshot,
                        service,
                        repository,
                        cold.generation,
                        &mut evidence,
                        &mut census,
                    )?;
                    evidence.finish(snapshot.document())?;
                    let census = census.finish(
                        &snapshot.document().diagnostics,
                        snapshot.document().skipped_regions.len(),
                    )?;
                    Ok((
                        rows,
                        snapshot.metadata().configuration_hash(),
                        json!({
                            "gap_census":census,
                            "exported_provenance":evidence.provenance,
                            "exported_fact_identities":evidence.fact_identities,
                            "exported_fact_identity_scope":"terminal_source_witnesses_not_semantic_records",
                            "exported_fact_root_scope":"emitted_normalized_files_gaps_matching_occurrences_and_bindings"
                        }),
                    ))
                },
            )??;
        let mut report = json!({
            "schema":if *report_mode == ReportMode::Full { "rootlight-index-coverage-diagnostic/4" }
                else { "rootlight-index-source-accounting-diagnostic/1" }, "explicit_configuration":config.is_some(),
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
        let serde_json::Value::Object(details) = details else {
            return Err("report details are not an object".into());
        };
        report
            .as_object_mut()
            .ok_or("report is not an object")?
            .extend(details);
        match lifetime {
            ServiceLifetime::Fresh => {
                report["service_lifetime"] = json!("fresh");
                report["cold"] = serde_json::to_value(cold)?;
                report["cold_wall_micros"] = cold_timing["wall_micros"].clone();
                report["cold_timing"] = cold_timing;
            }
            ServiceLifetime::Reopened | ServiceLifetime::Warm => {
                report["schema"] = json!(if *report_mode == ReportMode::Full {
                    "rootlight-index-update-diagnostic/1"
                } else {
                    "rootlight-index-source-accounting-update-diagnostic/1"
                });
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

fn index_with_optional_analysis_profile(
    service: &mut FirstSliceService,
    repository: &Path,
    enabled: bool,
) -> Result<(rootlight_service::FirstSliceIndexReceipt, serde_json::Value), Box<dyn Error>> {
    if !enabled {
        return index_with_timing(service, repository);
    }
    let (result, profile) =
        capture_first_slice_analysis_profile(|| index_with_timing(service, repository));
    let profile = serde_json::to_value(profile).unwrap_or_else(|_| {
        json!({
            "events_complete":false,
            "counts_complete":false,
            "serialization_complete":false,
            "error_code":"profile_serialization_failed"
        })
    });
    finish_profiled_index_with_writer(result, profile, || std::io::stderr().lock())
}

fn finish_profiled_index_with_writer<Writer: Write>(
    result: Result<(rootlight_service::FirstSliceIndexReceipt, serde_json::Value), Box<dyn Error>>,
    profile: serde_json::Value,
    errors: impl FnOnce() -> Writer,
) -> Result<(rootlight_service::FirstSliceIndexReceipt, serde_json::Value), Box<dyn Error>> {
    match result {
        Ok((receipt, mut timing)) => {
            timing["analysis_profile"] = profile;
            Ok((receipt, timing))
        }
        Err(error) => {
            // Diagnostic output failure cannot replace the original indexing result.
            let record = json!({"schema":"rootlight-index-analysis-profile-failure/1", "index_result":"error", "profile_scope":"partial_failed_operation", "analysis_profile":profile});
            let mut errors = errors();
            let _ = serde_json::to_writer(&mut errors, &record)
                .and_then(|()| errors.write_all(b"\n").map_err(serde_json::Error::io));
            Err(error)
        }
    }
}

fn index_with_timing(
    service: &mut FirstSliceService,
    repository: &Path,
) -> Result<(rootlight_service::FirstSliceIndexReceipt, serde_json::Value), Box<dyn Error>> {
    let cancellation = Cancellation::new();
    let started = Instant::now();
    let mut timeline = Vec::new();
    let prepared = service
        .prepare_repository_with_mode_and_progress(
            repository,
            FirstSliceIndexMode::Structural,
            &cancellation,
            |progress: FirstSliceIndexProgress| {
                timeline.push((Instant::now(), progress));
            },
        )
        .map_err(|error| {
            format!(
                "preparation failed after {} microseconds; last progress: {:?}; cause: {error}",
                started.elapsed().as_micros(),
                timeline.last().map(|(_, progress)| progress)
            )
        })?;
    let prepared_at = Instant::now();
    let commit = service
        .publish_prepared_with_metrics(prepared, &cancellation)
        .map_err(|error| format!("publication failed: {error}"))?;
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

fn occurrence_matches_gap(site: &OccurrenceRecord, gap: &SkippedRegion) -> bool {
    site.source.span() == gap.source.span()
        && site.source.repository() == gap.source.repository()
        && site.source.generation() == gap.source.generation()
        && site.source.content_hash() == gap.source.content_hash()
}

fn source_origins(
    document: &rootlight_ir::NormalizedIrDocument,
) -> Result<BTreeMap<rootlight_ids::FileId, &SourceRef>, Box<dyn Error>> {
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
    Ok(origins)
}

struct VerifiedSource {
    source_exact: bool,
    read_bytes: u64,
    read_chunks: usize,
    gap_text: Option<gap_text::GapTexts>,
}

struct FileSourceVerifier<'a> {
    snapshot: &'a rootlight_storage::GenerationSnapshot,
    service: &'a FirstSliceService,
    repository: &'a Path,
    generation: rootlight_ids::GenerationId,
    verified_containers: BTreeSet<rootlight_ids::FileId>,
}

impl<'a> FileSourceVerifier<'a> {
    fn new(
        snapshot: &'a rootlight_storage::GenerationSnapshot,
        service: &'a FirstSliceService,
        repository: &'a Path,
        generation: rootlight_ids::GenerationId,
    ) -> Self {
        Self {
            snapshot,
            service,
            repository,
            generation,
            verified_containers: BTreeSet::new(),
        }
    }

    fn verify(
        &mut self,
        file: &FileRecord,
        origin: Option<&SourceRef>,
        gap_spans: Option<Vec<(usize, usize)>>,
    ) -> Result<VerifiedSource, Box<dyn Error>> {
        let source = file
            .evidence
            .source
            .clone()
            .ok_or("file has no generation-bound source")?;
        if source.generation() != self.generation {
            return Err("file source belongs to a different generation".into());
        }
        if let Some(container_ref) = origin {
            let container = self
                .snapshot
                .find_file(container_ref.span().file())
                .ok_or("extracted source has no retained container")?;
            if container_ref.repository() != file.repository
                || container_ref.generation() != self.generation
                || container_ref.span().start_byte() != 0
                || container_ref.span().end_byte() != container.byte_length
                || container_ref.content_hash() != container.content_hash
            {
                return Err("extracted source has an invalid container reference".into());
            }
            if self.verified_containers.insert(container.id) {
                read_live_file(self.repository, container)?;
            }
        }
        let live = origin
            .is_none()
            .then(|| read_live_file(self.repository, file))
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
        let mut gap_text = gap_spans
            .map(|spans| gap_text::GapTexts::new(file_length, file.encoding == "utf-8", spans))
            .transpose()?;
        let mut returned_hasher = blake3::Hasher::new();
        let mut read_bytes = 0_u64;
        let mut read_chunks = 0_usize;
        let mut start = 0_usize;
        loop {
            let end = text.map_or_else(
                || start.saturating_add(64 * 1024).min(file_length),
                |text| chunk_end(text, start),
            );
            let part = SourceRef::new(
                source.repository(),
                source.generation(),
                SourceSpan::new(file.id, u64::try_from(start)?, u64::try_from(end)?)?,
                source.content_hash(),
                None,
            );
            let read = self.service.source_read_with_options_and_budget(
                self.generation,
                vec![part.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(encoding),
                FirstSliceBudget::default(),
                &Cancellation::new(),
            )?;
            if read.data.generation != self.generation || read.data.chunks.len() != 1 {
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
            read_bytes = read_bytes
                .checked_add(u64::try_from(chunk.bytes.len())?)
                .ok_or("source read byte count overflow")?;
            read_chunks = read_chunks
                .checked_add(1)
                .ok_or("source read chunk count overflow")?;
            if let Some(text) = &mut gap_text {
                text.capture(start, &chunk.bytes)?;
            }
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
        if read_bytes != file.byte_length {
            return Err("retrieved source byte count differs from indexed file".into());
        }
        Ok(VerifiedSource {
            source_exact: live.is_some(),
            read_bytes,
            read_chunks,
            gap_text,
        })
    }
}

#[derive(Default)]
struct FileRecordCounts {
    gaps: usize,
    occurrences: usize,
    resolved: usize,
    candidates: usize,
    unresolved: usize,
}

fn add_count(count: &mut usize, increment: usize) -> Result<(), Box<dyn Error>> {
    *count = count
        .checked_add(increment)
        .ok_or("retained record count overflow")?;
    Ok(())
}

fn verify_counted_source(
    file: &FileRecord,
    source: &SourceRef,
    generation: rootlight_ids::GenerationId,
) -> Result<(), Box<dyn Error>> {
    if source.repository() != file.repository
        || source.generation() != generation
        || source.content_hash() != file.content_hash
        || source.span().file() != file.id
        || source.span().end_byte() > file.byte_length
    {
        return Err("counted record source differs from retained file".into());
    }
    Ok(())
}

fn inspect_aggregate_source_recovery(
    document: &rootlight_ir::NormalizedIrDocument,
    metadata: rootlight_storage::GenerationMetadata,
    retained_file_count: usize,
) -> Result<Option<rootlight_ir::AggregateSourceRecovery>, Box<dyn Error>> {
    if document.repository != metadata.repository() || document.generation != metadata.generation()
    {
        return Err("aggregate recovery document differs from inspected generation".into());
    }
    let mut envelopes = document.extensions.iter().filter(|extension| {
        extension.namespace == rootlight_ir::AGGREGATE_SOURCE_RECOVERY_NAMESPACE
    });
    let Some(envelope) = envelopes.next() else {
        return Ok(None);
    };
    if envelopes.next().is_some()
        || envelope.repository != document.repository
        || envelope.generation != document.generation
    {
        return Err("aggregate recovery envelope differs from inspected generation".into());
    }
    let cause = rootlight_ir::decode_aggregate_source_recovery_envelope(envelope)?;
    // Recovery keeps one normalized anchor and the remaining members in the source catalog.
    if cause.source_inventory != metadata.manifest_hash()
        || u64::try_from(retained_file_count).ok() != Some(cause.members)
    {
        return Err("aggregate recovery inventory differs from inspected generation".into());
    }
    Ok(Some(cause))
}

fn inspect_source_files(
    snapshot: &rootlight_storage::GenerationSnapshot,
    service: &FirstSliceService,
    repository: &Path,
    generation: rootlight_ids::GenerationId,
) -> Result<(Vec<serde_json::Value>, serde_json::Value), Box<dyn Error>> {
    let document = snapshot.document();
    let origins = source_origins(document)?;
    let mut files = BTreeMap::new();
    for (file, normalized) in document.files.iter().map(|file| (file, true)).chain(
        snapshot
            .source_files()
            .entries()
            .iter()
            .map(|entry| (entry.file(), false)),
    ) {
        if files
            .insert(file.id, (file, normalized, FileRecordCounts::default()))
            .is_some()
        {
            return Err("retained file identity appears more than once".into());
        }
    }
    if files.len() != snapshot.file_count() {
        return Err("source accounting omitted a retained file record".into());
    }
    // Count retained records once; no target/provenance closure or per-record
    // witness payload is needed to account for physical source retention.
    for occurrence in &document.occurrences {
        let (file, _, counts) = files
            .get_mut(&occurrence.source.span().file())
            .ok_or("occurrence has no retained source file")?;
        verify_counted_source(file, &occurrence.source, generation)?;
        if occurrence.file != file.id
            || occurrence.repository != file.repository
            || occurrence.generation != generation
        {
            return Err("occurrence identity differs from counted source file".into());
        }
        add_count(&mut counts.occurrences, 1)?;
        match occurrence.target {
            OccurrenceTarget::Resolved { .. } => add_count(&mut counts.resolved, 1)?,
            OccurrenceTarget::Candidates { .. } => add_count(&mut counts.candidates, 1)?,
            OccurrenceTarget::Unresolved { .. } => add_count(&mut counts.unresolved, 1)?,
        }
    }
    for gap in &document.skipped_regions {
        let (file, _, counts) = files
            .get_mut(&gap.source.span().file())
            .ok_or("gap has no retained source file")?;
        verify_counted_source(file, &gap.source, generation)?;
        if gap.repository != file.repository || gap.generation != generation {
            return Err("gap identity differs from counted source file".into());
        }
        add_count(&mut counts.gaps, 1)?;
    }
    let mut verifier = FileSourceVerifier::new(snapshot, service, repository, generation);
    let mut rows = Vec::new();
    let mut gap_total = 0_usize;
    let mut occurrence_total = 0_usize;
    let mut physical_files = 0_usize;
    let mut normalized_files = 0_usize;
    let mut read_bytes = 0_u64;
    let mut read_chunks = 0_usize;
    for (file, normalized, counts) in files.values() {
        let origin = origins.get(&file.id).copied();
        let verified = verifier.verify(file, origin, None)?;
        if verified.gap_text.is_some() {
            return Err("source accounting constructed gap text witnesses".into());
        }
        add_count(&mut gap_total, counts.gaps)?;
        add_count(&mut occurrence_total, counts.occurrences)?;
        add_count(&mut read_chunks, verified.read_chunks)?;
        if origin.is_none() {
            add_count(&mut physical_files, 1)?;
        }
        if *normalized {
            add_count(&mut normalized_files, 1)?;
        }
        read_bytes = read_bytes
            .checked_add(verified.read_bytes)
            .ok_or("source read byte count overflow")?;
        let mut resolution_total = counts.resolved;
        add_count(&mut resolution_total, counts.candidates)?;
        add_count(&mut resolution_total, counts.unresolved)?;
        if resolution_total != counts.occurrences {
            return Err("occurrence resolution counts are not conserved".into());
        }
        let container_path = origin.and_then(|reference| {
            snapshot
                .find_file(reference.span().file())
                .map(|container| container.path.as_str())
        });
        rows.push(json!({
            "file":file,
            "normalized_record_present":normalized,
            "normalized_proof_status":if *normalized { "not_exported_source_accounting" }
                else { "unavailable_source_only" },
            "source_exact":verified.source_exact,
            "source_read_exact":true,
            "source_read_bytes":verified.read_bytes,
            "source_read_chunks":verified.read_chunks,
            "extracted_from":origin,
            "container_path":container_path,
            "container_live_exact":origin.map(|_| true),
            "extraction_independently_verified":origin.map(|_| false),
            "record_count_scope":"retained_normalized_records_not_source_completeness",
            "gap_record_count":counts.gaps,
            "occurrence_record_count":counts.occurrences,
            "occurrence_resolution_counts":{"resolved":counts.resolved,"candidates":counts.candidates,
                "unresolved":counts.unresolved},
            "gaps":null,"occurrences":null,"supplemental_calls":null,"external_bindings":null
        }));
    }
    if gap_total != document.skipped_regions.len() || occurrence_total != document.occurrences.len()
    {
        return Err("source accounting did not conserve retained normalized record counts".into());
    }
    let extracted_files = rows
        .len()
        .checked_sub(physical_files)
        .ok_or("extracted file count underflow")?;
    let source_only_files = rows
        .len()
        .checked_sub(normalized_files)
        .ok_or("source-only file count underflow")?;
    let accounting = json!({
        "scope":"retained_normalized_record_counts_not_exhaustive_semantic_denominators",
        "retained_file_records":rows.len(),"physical_file_records":physical_files,
        "extracted_file_records":extracted_files,"normalized_file_records":normalized_files,
        "source_only_file_records":source_only_files,
        "source_read_verified_file_records":rows.len(),"source_read_bytes":read_bytes,
        "source_read_chunks":read_chunks,"retained_gap_records":gap_total,
        "retained_occurrence_records":occurrence_total,
        "retained_entity_records":document.entities.len(),"retained_relation_records":document.relations.len(),
        "normalized_witness_details_exported":false,"full_semantic_completeness_proven":false,
        "installed_mcp_qualified":false
    });
    Ok((rows, accounting))
}

fn inspect_files<'snapshot>(
    snapshot: &'snapshot rootlight_storage::GenerationSnapshot,
    service: &FirstSliceService,
    repository: &Path,
    generation: rootlight_ids::GenerationId,
    evidence: &mut binding_proofs::BindingEvidence,
    census: &mut gap_census::GapCensus<'snapshot>,
) -> Result<Vec<serde_json::Value>, Box<dyn Error>> {
    let document = snapshot.document();
    let origins = source_origins(document)?;
    let mut verifier = FileSourceVerifier::new(snapshot, service, repository, generation);
    let mut rows = Vec::new();
    for (file, normalized) in document.files.iter().map(|file| (file, true)).chain(
        snapshot
            .source_files()
            .entries()
            .iter()
            .map(|entry| (entry.file(), false)),
    ) {
        let origin = origins.get(&file.id).copied();
        let gap_spans = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.source.span().file() == file.id)
            .map(|gap| {
                if gap.source.repository() != file.repository
                    || gap.source.generation() != generation
                    || gap.source.content_hash() != file.content_hash
                {
                    return Err("gap source identity differs from indexed file".into());
                }
                Ok((
                    usize::try_from(gap.source.span().start_byte())?,
                    usize::try_from(gap.source.span().end_byte())?,
                ))
            })
            .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        let verified = verifier.verify(file, origin, Some(gap_spans))?;
        let gap_text = verified
            .gap_text
            .as_ref()
            .ok_or("full report has no gap text collector")?;
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
                let candidates = sites_by_span
                    .get(&gap.source.span())
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                census.record(&file.language, gap, candidates)?;
                let mut value = serde_json::to_value(gap)?;
                evidence.admit_gap(gap);
                // A callee and its application can share a span but disagree on
                // resolution. Retain all matches instead of guessing gap ownership.
                let matching: Vec<_> = sites_by_span
                    .get(&gap.source.span())
                    .into_iter()
                    .flatten()
                    .filter(|site| occurrence_matches_gap(site, gap))
                    .map(|site| {
                        let target_provenance = evidence.admit_occurrence(document, site)?;
                        Ok::<_, Box<dyn Error>>(json!({
                            "id":site.id, "syntax_kind":site.syntax_kind,
                            "role":site.role, "target":site.target, "provenance":site.provenance,
                            "target_provenance":target_provenance,
                        }))
                    })
                    .collect::<Result<_, _>>()?;
                value["matching_occurrences"] = json!(matching);
                value["source_text"] = gap_text.evidence(
                    usize::try_from(gap.source.span().start_byte())?,
                    usize::try_from(gap.source.span().end_byte())?,
                )?;
                Ok(value)
            })
            .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        let container_path = origin.and_then(|reference| {
            snapshot
                .find_file(reference.span().file())
                .map(|container| container.path.as_str())
        });
        if normalized {
            evidence.admit_file(file);
        }
        rows.push(json!({
            "file":file,
            "normalized_proof_status":if normalized { "exported" } else { "unavailable_source_only" },
            "source_exact":verified.source_exact,
            "source_read_exact":true,
            "extracted_from":origin,
            "container_path":container_path,
            "container_live_exact":origin.map(|_| true),
            "extraction_independently_verified":origin.map(|_| false),
            "occurrences":occurrences,
            "gaps":gaps,
            "supplemental_calls":evidence.supplemental_calls.remove(&file.id).unwrap_or_default(),
            "external_bindings":evidence.external_bindings.remove(&file.id).unwrap_or_default()
        }));
    }
    if rows.len() != snapshot.file_count() {
        return Err("inspection omitted a retained file record".into());
    }
    if !evidence.external_bindings.is_empty() || !evidence.supplemental_calls.is_empty() {
        return Err("binding evidence has no inspected file".into());
    }
    Ok(rows)
}

fn chunk_end(text: &str, start: usize) -> usize {
    let mut end = start.saturating_add(64 * 1024).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    type ProofKey = (String, String);

    fn tempdir() -> tempfile::TempDir {
        // macOS exposes its default temporary directory through `/var`; SQLite's
        // no-follow boundary requires the real parent even for private fixtures.
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        tempfile::tempdir_in(parent).unwrap()
    }

    #[derive(Debug)]
    struct OriginalIndexCancellation {
        reason: rootlight_service::CancellationReason,
    }

    impl std::fmt::Display for OriginalIndexCancellation {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "original indexing cancellation: {:?}",
                self.reason
            )
        }
    }

    impl Error for OriginalIndexCancellation {}

    struct ProfileWriter {
        bytes: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
        reject_json: bool,
        reject_newline: bool,
    }

    impl Write for ProfileWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.reject_json || (self.reject_newline && bytes == b"\n") {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "controlled profile output failure",
                ));
            }
            self.bytes.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn analysis_profile_failure_output_preserves_original_error_on_json_or_newline_failure() {
        for (reject_json, reject_newline) in [(true, false), (false, true), (false, false)] {
            let original = Box::new(OriginalIndexCancellation {
                reason: rootlight_service::CancellationReason::ClientRequest,
            });
            let address = std::ptr::from_ref(original.as_ref());
            let bytes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let writer = ProfileWriter {
                bytes: std::rc::Rc::clone(&bytes),
                reject_json,
                reject_newline,
            };
            let result = finish_profiled_index_with_writer(
                Err(original),
                json!({"events_complete":false,"counts_complete":false}),
                || writer,
            );
            let returned = result
                .unwrap_err()
                .downcast::<OriginalIndexCancellation>()
                .unwrap();
            assert!(std::ptr::eq(address, returned.as_ref()));
            assert_eq!(
                returned.reason,
                rootlight_service::CancellationReason::ClientRequest
            );
            let bytes = bytes.borrow();
            if reject_json {
                assert!(bytes.is_empty());
            } else {
                let line: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(line["schema"], "rootlight-index-analysis-profile-failure/1");
                assert_eq!(line["index_result"], "error");
                assert_eq!(line["analysis_profile"]["events_complete"], false);
                assert_eq!(bytes.ends_with(b"\n"), !reject_newline);
            }
        }
    }

    #[test]
    fn analysis_profile_success_preserves_receipt_and_timing_without_acquiring_stderr() {
        let directory = tempdir();
        fs::write(directory.path().join("lib.rs"), "pub fn profiled() {}\n").unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let (receipt, timing) = index_with_timing(&mut service, directory.path()).unwrap();
        assert!(timing.get("analysis_profile").is_none());
        let profile = json!({"events_complete":true,"counts_complete":true});
        let (returned, mut updated) = finish_profiled_index_with_writer(
            Ok((receipt.clone(), timing.clone())),
            profile.clone(),
            || -> Vec<u8> { panic!("successful indexing must not acquire the error writer") },
        )
        .unwrap();
        assert_eq!(returned, receipt);
        assert_eq!(
            updated
                .as_object_mut()
                .unwrap()
                .remove("analysis_profile")
                .unwrap(),
            profile
        );
        assert_eq!(updated, timing);
    }

    #[test]
    fn analysis_profile_flag_is_opt_in_and_preserves_mode_validation() {
        for flags in [
            vec![],
            vec!["--resume"],
            vec!["--warm-sequence"],
            vec!["--source-accounting"],
        ] {
            let args: Vec<_> = flags
                .into_iter()
                .chain(["repository", "state"])
                .map(OsString::from)
                .collect();
            assert!(!parse_arguments(&args).unwrap().analysis_profile);
        }
        for flags in [
            vec!["--analysis-profile"],
            vec!["--resume", "--analysis-profile"],
            vec!["--analysis-profile", "--warm-sequence"],
            vec!["--source-accounting", "--analysis-profile"],
        ] {
            let args: Vec<_> = flags
                .into_iter()
                .chain(["repository", "state"])
                .map(OsString::from)
                .collect();
            assert!(parse_arguments(&args).unwrap().analysis_profile);
        }
        for flags in [
            vec!["--analysis-profile", "--analysis-profile"],
            vec!["--analysis-profile", "--resume", "--warm-sequence"],
            vec![
                "--analysis-profile",
                "--source-accounting",
                "--warm-sequence",
            ],
        ] {
            let args: Vec<_> = flags
                .into_iter()
                .chain(["repository", "state"])
                .map(OsString::from)
                .collect();
            assert!(parse_arguments(&args).is_err());
        }
    }

    #[test]
    fn analysis_profile_is_absent_by_default_in_fresh_warm_and_resume_reports() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        let state = directory.path().join("state");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("lib.rs"), "pub fn profiled() -> u8 { 1 }\n").unwrap();
        let mut session =
            DiagnosticSession::open(&repository, &state, None, StateMode::Fresh).unwrap();
        let fresh = session.inspect().unwrap();
        assert_eq!(fresh["schema"], "rootlight-index-coverage-diagnostic/4");
        assert!(fresh["cold_timing"].get("analysis_profile").is_none());
        assert!(fresh["no_op_timing"].get("analysis_profile").is_none());
        fs::write(repository.join("lib.rs"), "pub fn profiled() -> u8 { 2 }\n").unwrap();
        let warm = session.inspect().unwrap();
        assert_eq!(warm["schema"], "rootlight-index-warm-update-diagnostic/1");
        assert!(warm["update_timing"].get("analysis_profile").is_none());
        assert!(warm["no_op_timing"].get("analysis_profile").is_none());
        drop(session);
        let mut resumed =
            DiagnosticSession::open(&repository, &state, None, StateMode::Resume).unwrap();
        let report = resumed.inspect().unwrap();
        assert_eq!(report["schema"], "rootlight-index-update-diagnostic/1");
        assert!(report["update_timing"].get("analysis_profile").is_none());
        assert!(report["no_op_timing"].get("analysis_profile").is_none());
    }

    #[test]
    fn analysis_profile_reports_each_explicit_operation_and_noop_has_no_analysis_phases() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("lib.rs"), "pub fn profiled() -> u8 { 1 }\n").unwrap();
        let mut session = DiagnosticSession::open(
            &repository,
            &directory.path().join("state"),
            None,
            StateMode::Fresh,
        )
        .unwrap();
        session.analysis_profile = true;
        let initial = session.inspect().unwrap();
        assert_eq!(
            initial["cold_timing"]["analysis_profile"]["counts_complete"],
            true
        );
        assert!(
            !initial["cold_timing"]["analysis_profile"]["phases"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            initial["no_op_timing"]["analysis_profile"]["phases"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        fs::write(repository.join("lib.rs"), "pub fn profiled() -> u8 { 2 }\n").unwrap();
        let update = session.inspect().unwrap();
        assert_eq!(
            update["update_timing"]["analysis_profile"]["events_complete"],
            true
        );
        assert!(
            !update["update_timing"]["analysis_profile"]["phases"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            update["no_op_timing"]["analysis_profile"]["phases"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn failed_index_reports_preparation_context_without_a_success_receipt() {
        let directory = tempdir();
        let missing_repository = directory.path().join("missing");
        let mut service = FirstSliceService::new(2).unwrap();
        let cause = service
            .prepare_repository_with_mode_and_progress(
                &missing_repository,
                FirstSliceIndexMode::Structural,
                &Cancellation::new(),
                |_| {},
            )
            .err()
            .unwrap()
            .to_string();
        let error = index_with_timing(&mut service, &missing_repository)
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("preparation failed after "), "{error}");
        assert!(error.contains("last progress: None"), "{error}");
        assert!(error.ends_with(&format!("cause: {cause}")), "{error}");
    }

    fn inspected_document(repository: &Path, state: &Path) -> rootlight_ir::NormalizedIrDocument {
        let session = DiagnosticSession::open(repository, state, None, StateMode::Resume).unwrap();
        let registered = session
            .service
            .registered_repository_for_root(repository, &Cancellation::new())
            .unwrap()
            .unwrap();
        let generation = session
            .service
            .repository_status(registered, None)
            .unwrap()
            .resolved_generation;
        session
            .service
            .inspect_generation(
                registered,
                Some(generation),
                &Cancellation::new(),
                |snapshot| snapshot.document().clone(),
            )
            .unwrap()
    }

    fn emitted_field_roots(
        report: &serde_json::Value,
        document: &rootlight_ir::NormalizedIrDocument,
    ) -> BTreeSet<ProofKey> {
        let identities: BTreeMap<_, _> = report["exported_fact_identities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|identity| {
                (
                    (
                        identity["kind"].as_str().unwrap(),
                        identity["id"].as_str().unwrap(),
                    ),
                    identity,
                )
            })
            .collect();
        let proofs: BTreeMap<_, _> = report["exported_provenance"]
            .as_array()
            .unwrap()
            .iter()
            .map(|proof| (proof["id"].as_str().unwrap(), proof))
            .collect();
        let expected_proofs: BTreeMap<_, _> = document
            .provenance
            .iter()
            .map(|proof| (proof.id.to_string(), serde_json::to_value(proof).unwrap()))
            .collect();
        let mut roots = BTreeSet::new();
        let mut admit = |kind: &str, record: &serde_json::Value, subtype: Option<&str>| {
            let id = record["id"].as_str().unwrap();
            let source = record
                .get("source")
                .unwrap_or(&record["evidence"]["source"]);
            let mut expected = json!({
                "kind":kind, "id":id, "repository":record["repository"],
                "generation":record["generation"], "source":source,
            });
            if let Some(subtype) = subtype {
                expected["subtype"] = json!(subtype);
            }
            assert_eq!(
                identities[&(kind, id)],
                &expected,
                "emitted field identity {kind}:{id}"
            );
            let provenance = record["provenance"].as_str().unwrap();
            assert_eq!(proofs[provenance], &expected_proofs[provenance]);
            roots.extend([
                (kind.to_owned(), id.to_owned()),
                ("fact".to_owned(), provenance.to_owned()),
            ]);
        };
        for row in report["files"].as_array().unwrap() {
            let file = document
                .files
                .iter()
                .find(|file| json!(file.id) == row["file"]["id"]);
            if let Some(file) = file {
                assert_eq!(row["normalized_proof_status"], "exported");
                assert_eq!(row["file"], serde_json::to_value(file).unwrap());
                admit("file", &row["file"], None);
            } else {
                assert_eq!(row["normalized_proof_status"], "unavailable_source_only");
                assert!(!identities.contains_key(&("file", row["file"]["id"].as_str().unwrap())));
                assert!(row["gaps"].as_array().unwrap().is_empty());
            }
            for gap in row["gaps"].as_array().unwrap() {
                let actual = document
                    .skipped_regions
                    .iter()
                    .find(|record| json!(record.id) == gap["id"])
                    .unwrap();
                let mut normalized_gap = gap.clone();
                normalized_gap
                    .as_object_mut()
                    .unwrap()
                    .remove("matching_occurrences");
                normalized_gap
                    .as_object_mut()
                    .unwrap()
                    .remove("source_text");
                assert_eq!(normalized_gap, serde_json::to_value(actual).unwrap());
                admit("fact", &normalized_gap, Some("skipped_region"));
                for matching in gap["matching_occurrences"].as_array().unwrap() {
                    let site = document
                        .occurrences
                        .iter()
                        .find(|site| json!(site.id) == matching["id"])
                        .unwrap();
                    assert!(occurrence_matches_gap(site, actual));
                    let targets: &[rootlight_ids::SymbolId] = match &site.target {
                        OccurrenceTarget::Resolved { symbol } => std::slice::from_ref(symbol),
                        OccurrenceTarget::Candidates { symbols, .. } => symbols,
                        OccurrenceTarget::Unresolved { .. } => &[],
                    };
                    let target_provenance: Vec<_> = targets
                        .iter()
                        .map(|symbol| {
                            let entity = document
                                .entities
                                .iter()
                                .find(|entity| entity.id == *symbol)
                                .unwrap();
                            json!({"symbol":entity.id, "provenance":entity.provenance})
                        })
                        .collect();
                    assert_eq!(
                        *matching,
                        json!({
                            "id":site.id, "syntax_kind":site.syntax_kind, "role":site.role,
                            "target":site.target, "provenance":site.provenance,
                            "target_provenance":target_provenance,
                        })
                    );
                    admit(
                        "fact",
                        &serde_json::to_value(site).unwrap(),
                        Some("occurrence"),
                    );
                    for symbol in targets {
                        let entity = document
                            .entities
                            .iter()
                            .find(|entity| entity.id == *symbol)
                            .unwrap();
                        admit("entity", &serde_json::to_value(entity).unwrap(), None);
                    }
                }
            }
        }
        roots
    }

    fn artifact_field_roots(report: &serde_json::Value) -> BTreeSet<ProofKey> {
        let mut roots = BTreeSet::new();
        let mut admit = |kind: &str, id: &serde_json::Value, provenance: &serde_json::Value| {
            roots.extend([
                (kind.to_owned(), id.as_str().unwrap().to_owned()),
                ("fact".to_owned(), provenance.as_str().unwrap().to_owned()),
            ]);
        };
        for row in report["files"].as_array().unwrap() {
            if row["normalized_proof_status"] == "exported" {
                admit("file", &row["file"]["id"], &row["file"]["provenance"]);
            }
            for gap in row["gaps"].as_array().unwrap() {
                admit("fact", &gap["id"], &gap["provenance"]);
                for matching in gap["matching_occurrences"].as_array().unwrap() {
                    admit("fact", &matching["id"], &matching["provenance"]);
                    let targets: Vec<_> = match matching["target"]["kind"].as_str().unwrap() {
                        "resolved" => vec![matching["target"]["symbol"].as_str().unwrap()],
                        "candidates" => matching["target"]["symbols"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|id| id.as_str().unwrap())
                            .collect(),
                        "unresolved" => Vec::new(),
                        kind => panic!("unexpected target kind: {kind}"),
                    };
                    let witnesses = matching["target_provenance"].as_array().unwrap();
                    assert_eq!(witnesses.len(), targets.len());
                    for (witness, symbol) in witnesses.iter().zip(targets) {
                        assert_eq!(witness["symbol"], symbol);
                        admit("entity", &witness["symbol"], &witness["provenance"]);
                    }
                }
            }
        }
        roots
    }

    fn assert_exact_proof_closure(
        report: &serde_json::Value,
        document: &rootlight_ir::NormalizedIrDocument,
        mut pending: BTreeSet<ProofKey>,
    ) {
        let proofs: BTreeMap<_, _> = report["exported_provenance"]
            .as_array()
            .unwrap()
            .iter()
            .map(|proof| (proof["id"].as_str().unwrap().to_owned(), proof))
            .collect();
        let identities: BTreeMap<_, _> = report["exported_fact_identities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|identity| {
                (
                    (
                        identity["kind"].as_str().unwrap().to_owned(),
                        identity["id"].as_str().unwrap().to_owned(),
                    ),
                    identity,
                )
            })
            .collect();
        let expected_proofs: BTreeMap<_, _> = document
            .provenance
            .iter()
            .map(|proof| (proof.id.to_string(), serde_json::to_value(proof).unwrap()))
            .collect();
        assert_eq!(
            proofs.len(),
            report["exported_provenance"].as_array().unwrap().len()
        );
        assert_eq!(
            identities.len(),
            report["exported_fact_identities"].as_array().unwrap().len()
        );
        let mut visited = BTreeSet::new();
        while let Some(key) = pending.pop_first() {
            if !visited.insert(key.clone()) {
                continue;
            }
            if key.0 == "fact"
                && let Some(proof) = proofs.get(&key.1)
            {
                assert_eq!(*proof, &expected_proofs[&key.1]);
                for parent in proof["derivation_parents"].as_array().unwrap() {
                    pending.insert((
                        parent["kind"].as_str().unwrap().to_owned(),
                        parent["id"].as_str().unwrap().to_owned(),
                    ));
                }
                for source in proof["input_sources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .chain(proof["evidence_sources"].as_array().unwrap())
                {
                    pending.insert((
                        "file".to_owned(),
                        source["span"]["file"].as_str().unwrap().to_owned(),
                    ));
                }
            } else {
                let identity = identities
                    .get(&key)
                    .expect("all terminal source witnesses are exported");
                if !identity["source"].is_null() {
                    pending.insert((
                        "file".to_owned(),
                        identity["source"]["span"]["file"]
                            .as_str()
                            .unwrap()
                            .to_owned(),
                    ));
                }
            }
        }
        let exported: BTreeSet<_> = identities
            .keys()
            .cloned()
            .chain(proofs.keys().map(|id| ("fact".to_owned(), id.clone())))
            .collect();
        assert_eq!(
            visited, exported,
            "exact closure of admitted report fields, with no extra roots"
        );
    }

    #[test]
    fn emitted_gap_fields_export_exact_roots_without_binding_records() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("Module.bas"), "Attribute VB_Name = \"Module\"\nSub Visit(ByVal required As Long)\nEnd Sub\nSub Run()\nVisit\nEnd Sub\n").unwrap();
        fs::write(repository.join("notes.asset"), "unknown language source\n").unwrap();
        let state = directory.path().join("state");
        let report = inspect(&repository, &state, None, StateMode::Fresh).unwrap();
        assert_eq!(
            report["exported_fact_root_scope"],
            "emitted_normalized_files_gaps_matching_occurrences_and_bindings"
        );
        assert_eq!(report["full_semantic_completeness_proven"], false);
        assert_eq!(report["installed_mcp_qualified"], false);
        for row in report["files"].as_array().unwrap() {
            assert!(row["external_bindings"].as_array().unwrap().is_empty());
            assert!(row["supplemental_calls"].as_array().unwrap().is_empty());
            assert_eq!(row["source_read_exact"], true);
        }
        let module = report["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["file"]["path"] == "Module.bas")
            .unwrap();
        let matches = module["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|gap| gap["source_text"]["text"] == "Visit")
            .unwrap()["matching_occurrences"]
            .as_array()
            .unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0]["syntax_kind"], "vba.application.reference");
        assert_eq!(matches[0]["target"]["kind"], "unresolved");
        assert_eq!(
            matches[1]["syntax_kind"],
            "vba.invoked_identifier.reference"
        );
        assert_eq!(matches[1]["role"], "call_site");
        assert_eq!(matches[1]["target"]["kind"], "resolved");
        assert_ne!(matches[0]["id"], matches[1]["id"]);
        assert_ne!(matches[0]["role"], matches[1]["role"]);
        assert!(matches.iter().all(|site| site["provenance"].is_string()));
        assert_eq!(matches[0]["target_provenance"], json!([]));
        assert_eq!(matches[1]["target_provenance"].as_array().unwrap().len(), 1);
        assert_eq!(
            matches[1]["target_provenance"][0]["symbol"],
            matches[1]["target"]["symbol"]
        );
        let retained = report["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["file"]["path"] == "notes.asset")
            .unwrap();
        assert_eq!(retained["normalized_proof_status"], "exported");
        let document = inspected_document(&repository, &state);
        assert_eq!(
            report["gap_census"]["gaps"]["total_records"],
            document.skipped_regions.len(),
        );
        assert_eq!(
            report["gap_census"]["full_semantic_completeness_proven"],
            false
        );
        assert_eq!(report["gap_census"]["installed_mcp_qualified"], false);
        let notes = document
            .files
            .iter()
            .find(|file| file.path == "notes.asset")
            .unwrap();
        assert_eq!(retained["file"], serde_json::to_value(notes).unwrap());
        assert!(document.files.iter().all(|file| file.language != "haskell"));
        let roots = emitted_field_roots(&report, &document);
        assert_eq!(roots, artifact_field_roots(&report));
        assert!(!roots.is_empty());
        assert!(!report["exported_provenance"].as_array().unwrap().is_empty());
        assert_exact_proof_closure(&report, &document, artifact_field_roots(&report));
        let restored = inspect(&repository, &state, None, StateMode::Resume).unwrap();
        for field in [
            "files",
            "exported_provenance",
            "exported_fact_identities",
            "exported_fact_root_scope",
            "gap_census",
        ] {
            assert_eq!(report[field], restored[field], "reopened {field}");
        }
    }

    #[test]
    fn exact_gap_matching_rejects_other_spans_owners_and_content() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(
            repository.join("Module.bas"),
            "Attribute VB_Name = \"Module\"\nSub Run()\nMissing\nEnd Sub\n",
        )
        .unwrap();
        let state = directory.path().join("state");
        let report = inspect(&repository, &state, None, StateMode::Fresh).unwrap();
        let document = inspected_document(&repository, &state);
        let gap = document
            .skipped_regions
            .iter()
            .find(|gap| {
                document
                    .occurrences
                    .iter()
                    .any(|site| occurrence_matches_gap(site, gap))
            })
            .unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| occurrence_matches_gap(site, gap))
            .unwrap();
        assert!(occurrence_matches_gap(site, gap));
        let source = &site.source;
        let span = source.span();
        let other_repository = rootlight_ids::RepositoryId::from_bytes([201; 16]);
        let other_generation = rootlight_ids::GenerationId::from_bytes([202; 20]);
        let other_file = rootlight_ids::FileId::from_bytes([203; 20]);
        for changed in [
            SourceRef::new(
                other_repository,
                source.generation(),
                span,
                source.content_hash(),
                None,
            ),
            SourceRef::new(
                source.repository(),
                other_generation,
                span,
                source.content_hash(),
                None,
            ),
            SourceRef::new(
                source.repository(),
                source.generation(),
                span,
                rootlight_ids::content_hash(b"other"),
                None,
            ),
            SourceRef::new(
                source.repository(),
                source.generation(),
                SourceSpan::new(other_file, span.start_byte(), span.end_byte()).unwrap(),
                source.content_hash(),
                None,
            ),
            SourceRef::new(
                source.repository(),
                source.generation(),
                SourceSpan::new(span.file(), span.start_byte(), span.end_byte() + 1).unwrap(),
                source.content_hash(),
                None,
            ),
        ] {
            let mut changed_site = site.clone();
            changed_site.source = changed;
            assert!(!occurrence_matches_gap(&changed_site, gap));
        }
        assert_exact_proof_closure(&report, &document, emitted_field_roots(&report, &document));
    }

    #[test]
    fn candidate_field_targets_root_actual_entities_and_missing_targets_fail() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(
            repository.join("value.rs"),
            "pub fn first() {}\npub fn second() {}\n",
        )
        .unwrap();
        let state = directory.path().join("state");
        inspect(&repository, &state, None, StateMode::Fresh).unwrap();
        let mut document = inspected_document(&repository, &state);
        let targets: Vec<_> = document
            .entities
            .iter()
            .take(2)
            .map(|entity| entity.id)
            .collect();
        assert_eq!(targets.len(), 2);
        let mut site = document.occurrences.first().unwrap().clone();
        site.target = OccurrenceTarget::Candidates {
            symbols: targets.clone(),
            total_count: 2,
            completeness: rootlight_ir::CoverageStatus::Complete,
        };
        site.id = rootlight_ir::derive_occurrence_record_id(&site).unwrap();
        document.occurrences.push(site.clone());
        let mut evidence = binding_proofs::select_report_evidence(&document).unwrap();
        let witnesses = evidence.admit_occurrence(&document, &site).unwrap();
        let expected: Vec<_> = targets
            .iter()
            .map(|symbol| {
                let entity = document
                    .entities
                    .iter()
                    .find(|entity| entity.id == *symbol)
                    .unwrap();
                json!({"symbol":entity.id, "provenance":entity.provenance})
            })
            .collect();
        assert_eq!(
            witnesses, expected,
            "candidate order and actual provenance linkage"
        );
        evidence.finish(&document).unwrap();
        for id in &targets {
            let entity = document
                .entities
                .iter()
                .find(|entity| entity.id == *id)
                .unwrap();
            assert!(
                evidence
                    .fact_identities
                    .iter()
                    .any(|identity| identity["kind"] == "entity" && identity["id"] == json!(id))
            );
            assert!(
                evidence
                    .provenance
                    .iter()
                    .any(|proof| proof["id"] == json!(entity.provenance))
            );
        }
        document.entities.retain(|entity| entity.id != targets[0]);
        let failure = binding_proofs::select_report_evidence(&document)
            .unwrap()
            .admit_occurrence(&document, &site)
            .err()
            .unwrap();
        assert_eq!(
            failure.to_string(),
            "diagnostic occurrence has no target entity"
        );
    }

    #[test]
    fn normalized_file_field_proofs_reject_missing_owners_and_source_drift() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("value.rs"), "pub fn value() {}\n").unwrap();
        let state = directory.path().join("state");
        inspect(&repository, &state, None, StateMode::Fresh).unwrap();
        let original = inspected_document(&repository, &state);
        let file = original.files.first().unwrap();
        let check = |document: &rootlight_ir::NormalizedIrDocument| {
            let mut evidence = binding_proofs::select_report_evidence(document).unwrap();
            evidence.admit_file(file);
            evidence.finish(document).err().unwrap().to_string()
        };
        let mut missing = original.clone();
        missing
            .provenance
            .retain(|proof| proof.id != file.provenance);
        assert_eq!(
            check(&missing),
            "external binding proof has a missing parent"
        );
        let mut duplicate = original.clone();
        duplicate.files.push(file.clone());
        assert_eq!(
            check(&duplicate),
            "external binding proof has a duplicate identity"
        );
        let mut foreign = original.clone();
        foreign
            .provenance
            .iter_mut()
            .find(|proof| proof.id == file.provenance)
            .unwrap()
            .generation = rootlight_ids::GenerationId::from_bytes([204; 20]);
        assert_eq!(
            check(&foreign),
            "external binding proof belongs to a different generation"
        );
        let mut drift = original.clone();
        let changed = drift
            .files
            .iter_mut()
            .find(|entry| entry.id == file.id)
            .unwrap();
        let source = changed.evidence.source.as_ref().unwrap();
        changed.evidence.source = Some(SourceRef::new(
            source.repository(),
            source.generation(),
            source.span(),
            rootlight_ids::content_hash(b"other"),
            None,
        ));
        assert_eq!(
            check(&drift),
            "external binding proof source differs from its file identity"
        );
        let mut oversized = original.clone();
        let changed = oversized
            .files
            .iter_mut()
            .find(|entry| entry.id == file.id)
            .unwrap();
        let source = changed.evidence.source.as_ref().unwrap();
        changed.evidence.source = Some(SourceRef::new(
            source.repository(),
            source.generation(),
            SourceSpan::new(file.id, 0, file.byte_length + 1).unwrap(),
            source.content_hash(),
            None,
        ));
        assert_eq!(
            check(&oversized),
            "external binding proof source differs from its file identity"
        );
    }

    #[test]
    fn pretty_report_stream_preserves_json_and_propagates_write_errors() {
        let report = json!({"source":"λ", "values":[1, null, true]});
        let mut output = Vec::new();
        write_pretty_report(&report, &mut output).unwrap();
        assert_eq!(
            String::from_utf8(output.clone()).unwrap(),
            format!("{}\n", serde_json::to_string_pretty(&report).unwrap())
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
            report
        );
        struct RejectWrite;
        impl Write for RejectWrite {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("rejected write"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let failure = write_pretty_report(&report, &mut RejectWrite)
            .err()
            .unwrap();
        assert!(failure.to_string().contains("rejected write"));
        struct RejectFlush;
        impl Write for RejectFlush {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("rejected flush"))
            }
        }
        assert!(
            write_pretty_report(&report, &mut RejectFlush)
                .unwrap_err()
                .to_string()
                .contains("rejected flush")
        );
    }

    #[test]
    fn pretty_report_stream_buffers_small_output_fragments() {
        #[derive(Default)]
        struct CountWrites {
            bytes: Vec<u8>,
            calls: usize,
        }
        impl Write for CountWrites {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.calls += 1;
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let values: Vec<_> = (0..1_000)
            .map(|index| json!({"index":index, "source":"exact witness"}))
            .collect();
        let report = json!({"values":values});
        let mut output = CountWrites::default();
        write_pretty_report(&report, &mut output).unwrap();
        assert_eq!(
            output.bytes,
            format!("{}\n", serde_json::to_string_pretty(&report).unwrap()).as_bytes()
        );
        assert!(output.calls < values.len());
    }

    #[test]
    fn external_function_evidence_distinguishes_references_and_calls() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("book.xlsm"), fixture::book(&[("Client",
            b"Attribute VB_Name = \"Client\"\r\nSub Run()\r\nDim value As Variant\r\nvalue = VBA.Mid$(\"sample\", 1)\r\nEnd Sub\r\n")])).unwrap();
        let report = inspect(
            &repository,
            &directory.path().join("state"),
            None,
            StateMode::Fresh,
        )
        .unwrap();
        let records: Vec<_> = report["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|file| file["external_bindings"].as_array().unwrap())
            .collect();
        assert_eq!(records.len(), 3);
        assert_eq!(
            records
                .iter()
                .filter(|r| r["relation"]["predicate"] == "refers_to")
                .count(),
            2
        );
        let calls: Vec<_> = records
            .iter()
            .filter(|r| r["relation"]["predicate"] == "calls")
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["occurrence"]["role"], "call_site");
        assert_eq!(calls[0]["target"]["qualified_name"], "VBA.Mid$");
        assert!(calls[0]["target"]["evidence"]["source"].is_null());
        assert_eq!(
            calls[0]["relation"]["evidence"]["source"],
            calls[0]["occurrence"]["source"]
        );
        let proofs: BTreeMap<_, _> = report["exported_provenance"]
            .as_array()
            .unwrap()
            .iter()
            .map(|proof| (proof["id"].as_str().unwrap(), proof))
            .collect();
        assert_eq!(
            proofs.len(),
            report["exported_provenance"].as_array().unwrap().len()
        );
        let identities: BTreeMap<_, _> = report["exported_fact_identities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|identity| {
                (
                    (
                        identity["kind"].as_str().unwrap(),
                        identity["id"].as_str().unwrap(),
                    ),
                    identity,
                )
            })
            .collect();
        assert_eq!(
            identities.len(),
            report["exported_fact_identities"].as_array().unwrap().len()
        );
        for record in &records {
            for root in ["provenance", "target_provenance"] {
                assert_eq!(proofs[record[root]["id"].as_str().unwrap()], &record[root]);
            }
            assert!(proofs.contains_key(record["relation"]["provenance"].as_str().unwrap()));
        }
        for proof in proofs.values() {
            for parent in proof["derivation_parents"].as_array().unwrap() {
                let kind = parent["kind"].as_str().unwrap();
                let id = parent["id"].as_str().unwrap();
                assert!(
                    (kind == "fact" && proofs.contains_key(id))
                        || identities.contains_key(&(kind, id))
                );
            }
            for source in proof["input_sources"]
                .as_array()
                .unwrap()
                .iter()
                .chain(proof["evidence_sources"].as_array().unwrap())
            {
                assert!(
                    identities.contains_key(&("file", source["span"]["file"].as_str().unwrap()))
                );
            }
        }
        let callee = records
            .iter()
            .find(|record| {
                record["target"]["qualified_name"] == "VBA.Mid$"
                    && record["occurrence"]["role"] != "call_site"
            })
            .unwrap();
        let call_root = calls[0]["provenance"]["id"].as_str().unwrap();
        let callee_root = callee["provenance"]["id"].as_str().unwrap();
        assert_eq!(callee["occurrence"]["role"], "reference");
        assert_eq!(callee["target"]["id"], calls[0]["target"]["id"]);
        assert_eq!(callee["relation"]["predicate"], "refers_to");
        assert_eq!(callee["relation"]["object"]["id"], calls[0]["target"]["id"]);
        assert_eq!(
            callee["provenance"]["rule"],
            "vba-standard-function-v1.indexed_snapshot"
        );
        assert!(
            proofs[call_root]["evidence_sources"]
                .as_array()
                .unwrap()
                .contains(&callee["occurrence"]["source"])
        );
        // Application retains the callee span directly, not its resolver root
        // as a causal parent. Both independently exported branches share context.
        let mut branches = Vec::new();
        for root in [call_root, callee_root] {
            let mut pending = BTreeSet::from([root]);
            let mut reached = BTreeSet::new();
            while let Some(id) = pending.pop_first() {
                if !reached.insert(id) {
                    continue;
                }
                for parent in proofs[id]["derivation_parents"].as_array().unwrap() {
                    if parent["kind"] == "fact"
                        && proofs.contains_key(parent["id"].as_str().unwrap())
                    {
                        pending.insert(parent["id"].as_str().unwrap());
                    }
                }
            }
            branches.push(reached);
        }
        assert!(branches[0].intersection(&branches[1]).any(|id| {
            proofs[id]["rule"]
                .as_str()
                .unwrap()
                .starts_with("native-classic-project-membership-v1.")
        }));
        assert_eq!(
            report["exported_fact_identity_scope"],
            "terminal_source_witnesses_not_semantic_records"
        );
    }

    #[test]
    fn external_binding_proof_export_rejects_missing_ancestors_and_duplicate_ids() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("book.xlsm"), fixture::book(&[("Client",
            b"Attribute VB_Name = \"Client\"\r\nSub Run()\r\nDim value As Variant\r\nvalue = VBA.Mid$(\"sample\", 1)\r\nEnd Sub\r\n")])).unwrap();
        let mut session = DiagnosticSession::open(
            &repository,
            &directory.path().join("state"),
            None,
            StateMode::Fresh,
        )
        .unwrap();
        let (receipt, _) = index_with_timing(&mut session.service, &session.repository).unwrap();
        let mut document = session
            .service
            .inspect_generation(
                receipt.repository,
                Some(receipt.generation),
                &Cancellation::new(),
                |snapshot| snapshot.document().clone(),
            )
            .unwrap();
        let call_proof = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == rootlight_ir::OccurrenceRole::CallSite
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            })
            .unwrap()
            .provenance;
        let missing = rootlight_ids::derive_fact("diagnostic-missing-parent", b"missing").id();
        document
            .provenance
            .iter_mut()
            .find(|proof| proof.id == call_proof)
            .unwrap()
            .derivation_parents
            .push(rootlight_ir::FactRef::Fact(missing));
        let failure = binding_proofs::select_report_evidence(&document)
            .unwrap()
            .finish(&document)
            .err()
            .unwrap();
        assert_eq!(
            failure.to_string(),
            "external binding proof has a missing parent"
        );
        document
            .provenance
            .iter_mut()
            .find(|proof| proof.id == call_proof)
            .unwrap()
            .derivation_parents
            .retain(|parent| *parent != rootlight_ir::FactRef::Fact(missing));
        let duplicate = document.occurrences.first().unwrap().clone();
        document.occurrences.push(duplicate);
        let failure = binding_proofs::select_report_evidence(&document)
            .unwrap()
            .finish(&document)
            .err()
            .unwrap();
        assert_eq!(
            failure.to_string(),
            "external binding proof has a duplicate identity"
        );
    }

    #[test]
    fn external_bindings_export_exact_uses_without_repository_definitions() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("book.xlsm"), fixture::book(&[("Client",
            b"Attribute VB_Name = \"Client\"\r\nSub Run()\r\nDim text As String\r\ntext = VBA.vbTab\r\nEnd Sub\r\n")])).unwrap();
        let report = inspect(
            &repository,
            &directory.path().join("state"),
            None,
            StateMode::Fresh,
        )
        .unwrap();
        let records: Vec<_> = report["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|file| file["external_bindings"].as_array().unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        for record in records {
            assert!(record["target"]["evidence"]["source"].is_null());
            assert_eq!(record["target"]["kind"], "external_symbol");
            assert_eq!(
                record["occurrence"]["target"]["symbol"],
                record["target"]["id"]
            );
            assert_eq!(record["relation"]["predicate"], "refers_to");
            assert_eq!(
                record["relation"]["evidence"]["source"],
                record["occurrence"]["source"]
            );
            assert_eq!(
                record["target_provenance"]["rule"],
                "vba-standard-library-catalog-v1"
            );
        }
    }

    #[test]
    fn external_class_evidence_preserves_type_reference_and_call_roles() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("book.xlsm"), fixture::book(&[("Client",
            b"Attribute VB_Name = \"Client\"\r\nSub Inspect()\r\nDim items As Collection\r\nDim count As Long\r\nSet items = New Collection\r\ncount = items.Count\r\nEnd Sub\r\n")])).unwrap();
        let report = inspect(
            &repository,
            &directory.path().join("state"),
            None,
            StateMode::Fresh,
        )
        .unwrap();
        let records: Vec<_> = report["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|file| file["external_bindings"].as_array().unwrap())
            .collect();
        let mut roles = BTreeMap::new();
        for record in records {
            let role = record["occurrence"]["role"].as_str().unwrap();
            let expected = match role {
                "type_use" => "uses_type",
                "call_site" => "calls",
                "reference" => "refers_to",
                _ => panic!("unexpected role"),
            };
            assert_eq!(record["relation"]["predicate"], expected);
            assert_eq!(
                record["relation"]["evidence"]["source"],
                record["occurrence"]["source"]
            );
            assert!(record["target"]["evidence"]["source"].is_null());
            *roles.entry(role).or_insert(0) += 1;
        }
        assert_eq!(
            roles,
            BTreeMap::from([("type_use", 2), ("reference", 1), ("call_site", 1)])
        );
    }

    #[test]
    fn supplemental_calls_preserve_distinct_targets_and_conditions_after_reopen() {
        let directory = tempdir();
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
    fn internal_only_initializer_exports_exact_roots_and_complete_source_closure() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(
            repository.join("project.vbp"),
            "Type=Exe\nName=\"Project\"\nModule=Client; Client.bas\nClass=Worker; Worker.cls\n",
        )
        .unwrap();
        fs::write(repository.join("Client.bas"), "Attribute VB_Name = \"Client\"\nSub Run()\nDim item As New Worker\nDim target As Worker\nSet target = item\nEnd Sub\n").unwrap();
        fs::write(repository.join("Worker.cls"), "VERSION 1.0 CLASS\nAttribute VB_Name = \"Worker\"\nPrivate Sub Class_Initialize()\nEnd Sub\n").unwrap();
        let state = directory.path().join("state");
        let report = inspect(&repository, &state, None, StateMode::Fresh).unwrap();
        assert_eq!(
            report["files"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|row| row["external_bindings"].as_array().unwrap())
                .count(),
            0
        );
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
        assert_ne!(call["target"]["id"], call["lexical_target"]["id"]);

        let session =
            DiagnosticSession::open(&repository, &state, None, StateMode::Resume).unwrap();
        let registered = session
            .service
            .registered_repository_for_root(&repository, &Cancellation::new())
            .unwrap()
            .unwrap();
        let generation = session
            .service
            .repository_status(registered, None)
            .unwrap()
            .resolved_generation;
        let document = session
            .service
            .inspect_generation(
                registered,
                Some(generation),
                &Cancellation::new(),
                |snapshot| snapshot.document().clone(),
            )
            .unwrap();
        let expected_proofs: BTreeMap<_, _> = document
            .provenance
            .iter()
            .map(|proof| (proof.id.to_string(), serde_json::to_value(proof).unwrap()))
            .collect();
        let proofs: BTreeMap<_, _> = report["exported_provenance"]
            .as_array()
            .unwrap()
            .iter()
            .map(|proof| (proof["id"].as_str().unwrap().to_owned(), proof))
            .collect();
        let identities: BTreeMap<_, _> = report["exported_fact_identities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|identity| {
                (
                    (
                        identity["kind"].as_str().unwrap().to_owned(),
                        identity["id"].as_str().unwrap().to_owned(),
                    ),
                    identity,
                )
            })
            .collect();
        assert_eq!(
            proofs.len(),
            report["exported_provenance"].as_array().unwrap().len()
        );
        assert_eq!(
            identities.len(),
            report["exported_fact_identities"].as_array().unwrap().len()
        );
        assert_eq!(
            proofs[call["provenance"]["id"].as_str().unwrap()],
            &call["provenance"]
        );
        let mut pending = BTreeSet::new();
        for (name, kind, subtype) in [
            ("occurrence", "fact", Some("occurrence")),
            ("relation", "fact", Some("relation")),
            ("target", "entity", None),
            ("lexical_target", "entity", None),
        ] {
            let record = &call[name];
            let key = (kind.to_owned(), record["id"].as_str().unwrap().to_owned());
            let mut expected = json!({
                "kind":kind, "id":record["id"], "repository":record["repository"],
                "generation":record["generation"], "source":if name == "occurrence" { &record["source"] } else { &record["evidence"]["source"] },
            });
            if let Some(subtype) = subtype {
                expected["subtype"] = json!(subtype);
            }
            assert_eq!(identities[&key], &expected, "embedded root {name}");
            pending.insert(key);
            let id = record["provenance"].as_str().unwrap();
            assert_eq!(proofs[id], &expected_proofs[id], "root provenance {name}");
            pending.insert(("fact".to_owned(), id.to_owned()));
        }
        assert_eq!(
            emitted_field_roots(&report, &document),
            artifact_field_roots(&report)
        );
        pending.extend(artifact_field_roots(&report));
        let mut visited = BTreeSet::new();
        while let Some(key) = pending.pop_first() {
            if !visited.insert(key.clone()) {
                continue;
            }
            if key.0 == "fact"
                && let Some(proof) = proofs.get(&key.1)
            {
                assert_eq!(*proof, &expected_proofs[&key.1]);
                for parent in proof["derivation_parents"].as_array().unwrap() {
                    pending.insert((
                        parent["kind"].as_str().unwrap().to_owned(),
                        parent["id"].as_str().unwrap().to_owned(),
                    ));
                }
                for source in proof["input_sources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .chain(proof["evidence_sources"].as_array().unwrap())
                {
                    pending.insert((
                        "file".to_owned(),
                        source["span"]["file"].as_str().unwrap().to_owned(),
                    ));
                }
            } else {
                let identity = identities
                    .get(&key)
                    .expect("every terminal parent and source file is exported");
                let source = &identity["source"];
                if !source.is_null() {
                    pending.insert((
                        "file".to_owned(),
                        source["span"]["file"].as_str().unwrap().to_owned(),
                    ));
                }
            }
        }
        let exported: BTreeSet<_> = identities
            .keys()
            .cloned()
            .chain(proofs.keys().map(|id| ("fact".to_owned(), id.clone())))
            .collect();
        assert_eq!(
            visited, exported,
            "export contains exactly the closure of emitted roots"
        );
        assert_eq!(
            report["exported_fact_identity_scope"],
            "terminal_source_witnesses_not_semantic_records"
        );
        assert_eq!(report["full_semantic_completeness_proven"], false);
        assert_eq!(report["installed_mcp_qualified"], false);
        let restored = inspect(&repository, &state, None, StateMode::Resume).unwrap();
        assert_eq!(report["files"], restored["files"]);
        assert_eq!(
            report["exported_provenance"],
            restored["exported_provenance"]
        );
        assert_eq!(
            report["exported_fact_identities"],
            restored["exported_fact_identities"]
        );
    }

    #[test]
    fn gap_evidence_distinguishes_callees_applications_and_shared_spans() {
        let directory = tempdir();
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
                assert_eq!(gap["source_text"]["status"], "complete");
                assert_eq!(gap["source_text"]["text"], text);
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
        let directory = tempdir();
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
        let directory = tempdir();
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
        let directory = tempdir();
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
        let directory = tempdir();
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
            FirstSliceService::new_durable(2, paths.state_dir(), &Cancellation::new()).unwrap();
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
    fn source_accounting_flags_are_explicit_and_reject_conflicting_modes() {
        let parse = |args: &[&str]| {
            let args: Vec<_> = args
                .iter()
                .map(|argument| OsString::from(*argument))
                .collect();
            parse_arguments(&args).map(|parsed| {
                (
                    parsed.state_mode,
                    parsed.sequence,
                    parsed.report_mode,
                    parsed.paths.to_vec(),
                )
            })
        };
        for (flags, mode, report_mode, sequence) in [
            (vec![], StateMode::Fresh, ReportMode::Full, false),
            (vec!["--resume"], StateMode::Resume, ReportMode::Full, false),
            (
                vec!["--warm-sequence"],
                StateMode::Fresh,
                ReportMode::Full,
                true,
            ),
            (
                vec!["--source-accounting"],
                StateMode::Fresh,
                ReportMode::SourceAccounting,
                false,
            ),
            (
                vec!["--resume", "--source-accounting"],
                StateMode::Resume,
                ReportMode::SourceAccounting,
                false,
            ),
            (
                vec!["--source-accounting", "--resume"],
                StateMode::Resume,
                ReportMode::SourceAccounting,
                false,
            ),
        ] {
            let mut args = flags;
            args.extend(["repository", "state", "configuration"]);
            let (actual_mode, actual_sequence, actual_report, paths) = parse(&args).unwrap();
            assert_eq!(actual_mode, mode);
            assert_eq!(actual_sequence, sequence);
            assert_eq!(actual_report, report_mode);
            assert_eq!(
                paths,
                ["repository", "state", "configuration"].map(OsString::from)
            );
        }
        for args in [
            vec![],
            vec!["repository"],
            vec!["repository", "state", "configuration", "extra"],
            vec!["--unknown", "repository", "state"],
            vec!["--resume", "--resume", "repository", "state"],
            vec!["--warm-sequence", "--warm-sequence", "repository", "state"],
            vec!["--resume", "--warm-sequence", "repository", "state"],
            vec!["--warm-sequence", "--resume", "repository", "state"],
            vec![
                "--source-accounting",
                "--source-accounting",
                "repository",
                "state",
            ],
            vec![
                "--source-accounting",
                "--warm-sequence",
                "repository",
                "state",
            ],
            vec![
                "--warm-sequence",
                "--source-accounting",
                "repository",
                "state",
            ],
            vec!["repository", "--source-accounting", "state"],
            vec!["repository", "state", "--unknown"],
        ] {
            assert!(
                parse(&args).is_err(),
                "accepted invalid arguments: {args:?}"
            );
        }
        let mut count = usize::MAX;
        assert!(add_count(&mut count, 1).is_err());
        assert_eq!(count, usize::MAX);
    }

    fn assert_source_accounting_scope(report: &serde_json::Value) {
        assert_eq!(report["report_scope"], "retained_source_accounting");
        for key in [
            "gap_census",
            "exported_provenance",
            "exported_fact_identities",
        ] {
            assert!(
                report.get(key).unwrap().is_null(),
                "detail is not omitted: {key}"
            );
        }
        for key in [
            "gap_census_scope",
            "exported_fact_identity_scope",
            "exported_fact_root_scope",
        ] {
            assert_eq!(report[key], "not_exported_source_accounting");
        }
        for key in [
            "normalized_proof_exported",
            "structural_coverage_qualified",
            "semantic_resolution_qualified",
            "full_semantic_completeness_proven",
            "installed_mcp_qualified",
        ] {
            assert_eq!(report[key], false);
        }
        let rows = report["files"].as_array().unwrap();
        assert_eq!(
            report["source_accounting"]["retained_file_records"],
            rows.len()
        );
        assert_eq!(
            report["source_accounting"]["source_read_verified_file_records"],
            rows.len()
        );
        let mut gaps = 0_u64;
        let mut occurrences = 0_u64;
        let mut bytes = 0_u64;
        let mut chunks = 0_u64;
        for row in rows {
            for key in [
                "gaps",
                "occurrences",
                "supplemental_calls",
                "external_bindings",
            ] {
                assert!(row.get(key).unwrap().is_null());
            }
            assert_eq!(row["source_read_exact"], true);
            assert_ne!(row["normalized_proof_status"], "exported");
            assert_eq!(row["source_read_bytes"], row["file"]["byte_length"]);
            assert!(row["source_read_chunks"].as_u64().unwrap() >= 1);
            assert_eq!(
                row["record_count_scope"],
                "retained_normalized_records_not_source_completeness"
            );
            let resolution = &row["occurrence_resolution_counts"];
            assert_eq!(
                row["occurrence_record_count"].as_u64().unwrap(),
                resolution["resolved"].as_u64().unwrap()
                    + resolution["candidates"].as_u64().unwrap()
                    + resolution["unresolved"].as_u64().unwrap()
            );
            gaps += row["gap_record_count"].as_u64().unwrap();
            occurrences += row["occurrence_record_count"].as_u64().unwrap();
            bytes += row["source_read_bytes"].as_u64().unwrap();
            chunks += row["source_read_chunks"].as_u64().unwrap();
        }
        let accounting = &report["source_accounting"];
        assert_eq!(
            accounting["scope"],
            "retained_normalized_record_counts_not_exhaustive_semantic_denominators"
        );
        assert_eq!(accounting["retained_gap_records"], gaps);
        assert_eq!(accounting["retained_occurrence_records"], occurrences);
        assert_eq!(accounting["source_read_bytes"], bytes);
        assert_eq!(accounting["source_read_chunks"], chunks);
        assert_eq!(accounting["normalized_witness_details_exported"], false);
        assert_eq!(accounting["full_semantic_completeness_proven"], false);
        assert_eq!(accounting["installed_mcp_qualified"], false);
    }

    fn aggregate_recovery_projection_fixture() -> (
        rootlight_ir::NormalizedIrDocument,
        rootlight_storage::GenerationMetadata,
        usize,
        rootlight_ir::AggregateSourceRecovery,
    ) {
        let directory = tempdir();
        fs::write(directory.path().join("module.rs"), "pub fn sample() {}\n").unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(directory.path(), &Cancellation::new())
            .unwrap();
        let (mut document, metadata, file_count) = service
            .inspect_generation(
                receipt.repository,
                Some(receipt.generation),
                &Cancellation::new(),
                |snapshot| {
                    (
                        snapshot.document().clone(),
                        snapshot.metadata(),
                        snapshot.file_count(),
                    )
                },
            )
            .unwrap();
        let cause = rootlight_ir::AggregateSourceRecovery {
            source_inventory: metadata.manifest_hash(),
            members: u64::try_from(file_count).unwrap(),
            derived_members: 0,
            stage: rootlight_ir::AggregateCapacityStage::StructuralAppend,
            resource: rootlight_ir::AggregateCapacityResource::Entities,
            observed: 2,
            limit: 1,
            text_snapshot_bytes: 1024,
        };
        // This canonical-envelope fixture tests projection, not a claimed capacity event.
        set_projection_cause(&mut document, cause);
        (document, metadata, file_count, cause)
    }

    fn set_projection_cause(
        document: &mut rootlight_ir::NormalizedIrDocument,
        cause: rootlight_ir::AggregateSourceRecovery,
    ) {
        document.extensions.retain(|extension| {
            extension.namespace != rootlight_ir::AGGREGATE_SOURCE_RECOVERY_NAMESPACE
        });
        let anchor = &document.files[0];
        document.extensions.push(
            rootlight_ir::new_aggregate_source_recovery_envelope(
                document.repository,
                document.generation,
                anchor.provenance,
                anchor.evidence.source.clone().unwrap(),
                cause,
            )
            .unwrap(),
        );
    }

    #[test]
    fn aggregate_source_recovery_projection_preserves_exact_cause_without_analysis_claims() {
        let (document, metadata, file_count, cause) = aggregate_recovery_projection_fixture();
        let observed = inspect_aggregate_source_recovery(&document, metadata, file_count).unwrap();
        assert_eq!(observed, Some(cause));
        assert_eq!(
            serde_json::to_value(observed).unwrap(),
            json!({
                "source_inventory":metadata.manifest_hash(), "members":file_count,
                "derived_members":0, "stage":"structural_append", "resource":"entities",
                "observed":2, "limit":1, "text_snapshot_bytes":1024,
            })
        );
    }

    #[test]
    fn aggregate_source_recovery_projection_rejects_invalid_inventory_and_context_joins() {
        let (document, metadata, file_count, cause) = aggregate_recovery_projection_fixture();
        for invalid in [
            rootlight_ir::AggregateSourceRecovery {
                source_inventory: rootlight_ids::content_hash(b"foreign inventory"),
                ..cause
            },
            rootlight_ir::AggregateSourceRecovery {
                members: cause.members + 1,
                ..cause
            },
        ] {
            let mut changed = document.clone();
            set_projection_cause(&mut changed, invalid);
            let envelope = changed.extensions.last().unwrap();
            assert_eq!(
                rootlight_ir::decode_aggregate_source_recovery_envelope(envelope).unwrap(),
                invalid
            );
            assert!(inspect_aggregate_source_recovery(&changed, metadata, file_count).is_err());
        }
        let mut changed = document.clone();
        changed
            .extensions
            .push(document.extensions.last().unwrap().clone());
        assert!(inspect_aggregate_source_recovery(&changed, metadata, file_count).is_err());
        let mut changed = document.clone();
        changed.extensions.last_mut().unwrap().payload.push(' ');
        assert!(inspect_aggregate_source_recovery(&changed, metadata, file_count).is_err());
        let mut changed = document.clone();
        changed.repository = rootlight_ids::derive_repository(b"foreign repository").id();
        assert!(inspect_aggregate_source_recovery(&changed, metadata, file_count).is_err());
        let mut changed = document;
        changed.generation = rootlight_ids::derive_generation(rootlight_ids::GenerationIdentity {
            repository: metadata.repository(),
            parent: Some(metadata.generation()),
            manifest_hash: metadata.manifest_hash(),
            config_hash: metadata.configuration_hash(),
            provider_set_hash: metadata.provider_set_hash(),
            format_version: 1,
        })
        .id();
        assert!(inspect_aggregate_source_recovery(&changed, metadata, file_count).is_err());
    }

    #[test]
    fn source_accounting_reports_ordinary_recovery_absence_after_reopen() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("module.rs"), "pub fn sample() {}\n").unwrap();
        let state = directory.path().join("state");
        for mode in [StateMode::Fresh, StateMode::Resume] {
            let report = inspect_source_accounting(&repository, &state, None, mode).unwrap();
            assert_source_accounting_scope(&report);
            assert!(report.get("aggregate_source_recovery").unwrap().is_null());
        }
    }

    #[test]
    fn full_report_keeps_its_default_schema_and_field_contract() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(
            repository.join("module.rs"),
            b"pub fn compute(value: i32) -> i32 { value + 1 }\n",
        )
        .unwrap();
        let report = inspect(
            &repository,
            &directory.path().join("state"),
            None,
            StateMode::Fresh,
        )
        .unwrap();
        assert_eq!(report["schema"], "rootlight-index-coverage-diagnostic/4");
        let expected: BTreeSet<_> = [
            "schema",
            "explicit_configuration",
            "configuration_input_hash",
            "configuration_hash",
            "open_wall_micros",
            "process_id",
            "no_op_same_generation",
            "no_op_wall_micros",
            "no_op_timing",
            "generation_inspection_method",
            "shared_bundle_roundtrip",
            "coverage",
            "files",
            "full_semantic_completeness_proven",
            "gap_census",
            "exported_provenance",
            "exported_fact_identities",
            "exported_fact_identity_scope",
            "exported_fact_root_scope",
            "installed_mcp_qualified",
            "service_lifetime",
            "cold",
            "cold_wall_micros",
            "cold_timing",
        ]
        .into_iter()
        .collect();
        let actual: BTreeSet<_> = report
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(actual, expected);
        assert!(report["exported_provenance"].is_array());
        assert!(report["exported_fact_identities"].is_array());
        assert!(report["gap_census"].is_object());
        let expected_file: BTreeSet<_> = [
            "file",
            "normalized_proof_status",
            "source_exact",
            "source_read_exact",
            "extracted_from",
            "container_path",
            "container_live_exact",
            "extraction_independently_verified",
            "occurrences",
            "gaps",
            "supplemental_calls",
            "external_bindings",
        ]
        .into_iter()
        .collect();
        for row in report["files"].as_array().unwrap() {
            assert_eq!(
                row.as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>(),
                expected_file
            );
        }
    }

    #[test]
    fn source_accounting_reads_all_physical_bytes_and_conserves_retained_counts_after_reopen() {
        let directory = tempdir();
        let root = fs::canonicalize(directory.path()).unwrap();
        let repository = root.join("repository");
        let state = root.join("state");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("empty.txt"), b"").unwrap();
        fs::write(
            repository.join("notes.txt"),
            format!(
                "{}🦀\r\n{}",
                "x".repeat(64 * 1024 - 1),
                "tail".repeat(20_000)
            ),
        )
        .unwrap();
        let encoded = std::iter::once(0xfeff_u16)
            .chain("λ𝄞\r\n".repeat(15_000).encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        fs::write(repository.join("encoded.txt"), &encoded).unwrap();
        fs::write(
            repository.join("module.rs"),
            b"pub fn compute(value: i32) -> i32 { value + 1 }\n",
        )
        .unwrap();
        let started = Instant::now();
        let report =
            inspect_source_accounting(&repository, &state, None, StateMode::Fresh).unwrap();
        let observer_wall_micros = started.elapsed().as_micros();
        assert_source_accounting_scope(&report);
        assert_eq!(
            report["schema"],
            "rootlight-index-source-accounting-diagnostic/1"
        );
        assert_eq!(report["service_lifetime"], "fresh");
        assert_eq!(report["source_accounting"]["physical_file_records"], 4);
        assert_eq!(report["source_accounting"]["extracted_file_records"], 0);
        assert_eq!(report["no_op_same_generation"], true);
        let document = inspected_document(&repository, &state);
        assert_eq!(
            report["source_accounting"]["retained_gap_records"],
            document.skipped_regions.len()
        );
        assert_eq!(
            report["source_accounting"]["retained_occurrence_records"],
            document.occurrences.len()
        );
        assert_eq!(
            report["source_accounting"]["retained_entity_records"],
            document.entities.len()
        );
        assert_eq!(
            report["source_accounting"]["retained_relation_records"],
            document.relations.len()
        );
        let rows = report["files"].as_array().unwrap();
        assert_eq!(rows.len(), 4);
        for row in rows {
            let bytes = fs::read(repository.join(row["file"]["path"].as_str().unwrap())).unwrap();
            assert_eq!(row["source_exact"], true);
            assert_eq!(row["file"]["byte_length"], bytes.len());
            assert_eq!(
                row["file"]["content_hash"],
                rootlight_ids::content_hash(&bytes).to_string()
            );
            let id = row["file"]["id"].as_str().unwrap();
            assert_eq!(
                row["gap_record_count"],
                document
                    .skipped_regions
                    .iter()
                    .filter(|gap| gap.source.span().file().to_string() == id)
                    .count()
            );
            assert_eq!(
                row["occurrence_record_count"],
                document
                    .occurrences
                    .iter()
                    .filter(|site| site.source.span().file().to_string() == id)
                    .count()
            );
        }
        let empty = rows
            .iter()
            .find(|row| row["file"]["path"] == "empty.txt")
            .unwrap();
        assert_eq!(empty["source_read_chunks"], 1);
        assert_eq!(empty["source_read_bytes"], 0);
        eprintln!(
            "{}",
            json!({
                "control":"exact-source-accounting-windows",
                "observer_wall_micros":observer_wall_micros,
                "index_wall_micros":report["cold_wall_micros"],
                "noop_wall_micros":report["no_op_wall_micros"],
                "source_read_calls":report["source_accounting"]["source_read_chunks"],
                "source_read_bytes":report["source_accounting"]["source_read_bytes"],
                "files":rows.iter().map(|row| json!({
                    "path":row["file"]["path"],
                    "hash":row["file"]["content_hash"],
                    "bytes":row["source_read_bytes"],
                    "calls":row["source_read_chunks"]
                })).collect::<Vec<_>>()
            })
        );
        let notes = rows
            .iter()
            .find(|row| row["file"]["path"] == "notes.txt")
            .unwrap();
        assert_eq!(notes["source_read_chunks"], 3);
        let encoded_row = rows
            .iter()
            .find(|row| row["file"]["path"] == "encoded.txt")
            .unwrap();
        assert_eq!(encoded_row["file"]["encoding"], "utf-16le");
        assert_eq!(encoded_row["source_read_chunks"], 3);
        let reopened =
            inspect_source_accounting(&repository, &state, None, StateMode::Resume).unwrap();
        assert_source_accounting_scope(&reopened);
        assert_eq!(
            reopened["schema"],
            "rootlight-index-source-accounting-update-diagnostic/1"
        );
        assert_eq!(reopened["service_lifetime"], "reopened");
        assert_eq!(
            reopened["previous_generation"],
            report["cold"]["generation"]
        );
        assert_eq!(
            reopened["update"]["generation"],
            report["cold"]["generation"]
        );
        assert_eq!(reopened["files"], report["files"]);
        assert_eq!(reopened["source_accounting"], report["source_accounting"]);
        assert_eq!(reopened["coverage"], report["coverage"]);
        assert_eq!(reopened["configuration_hash"], report["configuration_hash"]);
        for report in [&report, &reopened] {
            assert_eq!(report["no_op_timing"]["operation"]["changed_files"], 0);
        }
        assert!(inspect_source_accounting(&repository, &state, None, StateMode::Fresh).is_err());
        let session =
            DiagnosticSession::open(&repository, &state, None, StateMode::Resume).unwrap();
        let registered = session
            .service
            .registered_repository_for_root(&repository, &Cancellation::new())
            .unwrap()
            .unwrap();
        let generation = session
            .service
            .repository_status(registered, None)
            .unwrap()
            .resolved_generation;
        for (name, truncate) in [("notes.txt", false), ("encoded.txt", true)] {
            let path = repository.join(name);
            let original = fs::read(&path).unwrap();
            let mut changed = original.clone();
            if truncate {
                changed.pop();
            } else {
                *changed.last_mut().unwrap() = b'!';
            }
            fs::write(&path, changed).unwrap();
            let stale = session
                .service
                .inspect_generation(
                    registered,
                    Some(generation),
                    &Cancellation::new(),
                    |snapshot| {
                        inspect_source_files(snapshot, &session.service, &repository, generation)
                    },
                )
                .unwrap();
            assert!(stale.is_err(), "changed source tail was accepted: {name}");
            fs::write(path, original).unwrap();
        }
    }

    #[test]
    fn source_accounting_preserves_explicit_configuration_and_routes_on_resume() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(
            repository.join("module.bas"),
            b"Attribute VB_Name = \"Metrics\"\r\nSub Run()\r\nEnd Sub\r\n",
        )
        .unwrap();
        let configured = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n";
        for (mode, config) in [("default", None), ("configured", Some(configured))] {
            let state = directory.path().join(mode);
            let report =
                inspect_source_accounting(&repository, &state, config, StateMode::Fresh).unwrap();
            assert_source_accounting_scope(&report);
            assert_eq!(report["explicit_configuration"], config.is_some());
            assert_eq!(
                report["configuration_input_hash"],
                rootlight_ids::content_hash(config.unwrap_or("").as_bytes()).to_string()
            );
            assert_eq!(
                report["files"][0]["file"]["language"],
                if config.is_some() {
                    "vba"
                } else {
                    "visual-basic-classic"
                }
            );
            let reopened =
                inspect_source_accounting(&repository, &state, config, StateMode::Resume).unwrap();
            assert_source_accounting_scope(&reopened);
            assert_eq!(reopened["configuration_hash"], report["configuration_hash"]);
            assert_eq!(
                reopened["configuration_input_hash"],
                report["configuration_input_hash"]
            );
            assert_eq!(reopened["files"], report["files"]);
            assert_eq!(
                reopened["update"]["generation"],
                report["cold"]["generation"]
            );
        }
    }

    #[test]
    fn source_accounting_preserves_extracted_sources_without_claiming_independent_expansion() {
        let directory = tempdir();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        let module = b"Attribute VB_Name = \"Client\"\r\nSub Run()\r\nEnd Sub\r\n";
        let container = fixture::book(&[("Client", module)]);
        fs::write(repository.join("book.xlsm"), &container).unwrap();
        let report = inspect_source_accounting(
            &repository,
            &directory.path().join("state"),
            None,
            StateMode::Fresh,
        )
        .unwrap();
        assert_source_accounting_scope(&report);
        assert_eq!(report["source_accounting"]["physical_file_records"], 1);
        let rows = report["files"].as_array().unwrap();
        let physical = rows
            .iter()
            .find(|row| row["extracted_from"].is_null())
            .unwrap();
        assert_eq!(physical["file"]["path"], "book.xlsm");
        assert_eq!(physical["source_exact"], true);
        assert_eq!(physical["source_read_bytes"], container.len());
        let extracted: Vec<_> = rows
            .iter()
            .filter(|row| !row["extracted_from"].is_null())
            .collect();
        assert!(!extracted.is_empty());
        assert_eq!(
            report["source_accounting"]["extracted_file_records"],
            extracted.len()
        );
        for row in extracted {
            assert_eq!(row["source_exact"], false);
            assert_eq!(row["container_live_exact"], true);
            assert_eq!(row["container_path"], "book.xlsm");
            assert_eq!(row["extraction_independently_verified"], false);
            assert_eq!(
                row["file"]["content_hash"],
                rootlight_ids::content_hash(module).to_string()
            );
            assert_eq!(row["source_read_bytes"], module.len());
        }
    }

    #[test]
    fn source_accounting_resume_reindexes_changed_bytes_and_rejects_stale_live_source_proof() {
        let directory = tempdir();
        let root = fs::canonicalize(directory.path()).unwrap();
        let repository = root.join("repository");
        let state = root.join("state");
        fs::create_dir(&repository).unwrap();
        let original = b"pub fn compute(value: i32) -> i32 { value + 1 }\n";
        let changed = b"pub fn compute(value: i32) -> i32 { value + 2 }\n";
        fs::write(repository.join("module.rs"), original).unwrap();
        assert!(inspect_source_accounting(&repository, &state, None, StateMode::Resume).is_err());
        let cold = inspect_source_accounting(&repository, &state, None, StateMode::Fresh).unwrap();
        fs::write(repository.join("module.rs"), changed).unwrap();
        let session =
            DiagnosticSession::open(&repository, &state, None, StateMode::Resume).unwrap();
        let registered = session
            .service
            .registered_repository_for_root(&repository, &Cancellation::new())
            .unwrap()
            .unwrap();
        let generation = session
            .service
            .repository_status(registered, None)
            .unwrap()
            .resolved_generation;
        let stale = session
            .service
            .inspect_generation(
                registered,
                Some(generation),
                &Cancellation::new(),
                |snapshot| {
                    inspect_source_files(snapshot, &session.service, &repository, generation)
                },
            )
            .unwrap();
        assert!(stale.is_err());
        drop(session);
        let updated =
            inspect_source_accounting(&repository, &state, None, StateMode::Resume).unwrap();
        assert_source_accounting_scope(&updated);
        assert_eq!(updated["previous_generation"], cold["cold"]["generation"]);
        assert_ne!(updated["update"]["generation"], cold["cold"]["generation"]);
        assert_eq!(
            updated["files"][0]["file"]["content_hash"],
            rootlight_ids::content_hash(changed).to_string()
        );
        assert_eq!(updated["update_timing"]["operation"]["changed_files"], 1);
        assert_eq!(updated["no_op_timing"]["operation"]["changed_files"], 0);
        let reopened =
            inspect_source_accounting(&repository, &state, None, StateMode::Resume).unwrap();
        assert_eq!(reopened["files"], updated["files"]);
        assert_eq!(
            reopened["update"]["generation"],
            updated["update"]["generation"]
        );
    }

    #[test]
    fn bounded_reads_reject_oversized_and_missing_files() {
        let directory = tempdir();
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
            let text = format!("{}{}\r\n", "x".repeat(64 * 1024 - 1), tail.repeat(20_000));
            let mut start = 0;
            let mut reconstructed = String::new();
            while start < text.len() {
                let end = chunk_end(&text, start);
                assert!(end > start && end - start <= 64 * 1024);
                reconstructed.push_str(text.get(start..end).unwrap());
                start = end;
            }
            assert_eq!(reconstructed, text);
        }
        assert_eq!(chunk_end("", 0), 0);
    }

    #[test]
    fn diagnostic_reads_multichunk_utf16_without_transcoding() {
        let directory = tempdir();
        let root = fs::canonicalize(directory.path()).unwrap();
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        for (name, little_endian) in [("little.txt", true), ("big.txt", false)] {
            let source = "λ𝄞\r\n".repeat(15_000);
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
        let directory = tempdir();
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
