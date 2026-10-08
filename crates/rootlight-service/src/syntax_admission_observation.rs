//! Test-only observation of the production syntax-demand admission boundary.
//! An owned thread-local guard captures source-free preflight metadata and stops
//! preparation before lowering; an unarmed hook does not change normal service work.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
    io::{self, Write},
    marker::PhantomData,
    rc::Rc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::AnalysisLimits;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId};
use serde::Serialize;

use super::{
    FirstSliceError, FirstSliceResource, FirstSliceService, MAX_FIRST_SLICE_STRUCTURAL_FACTS,
    RustSourceInput, StructuralArtifactCollector, StructuralFactRequirement,
    canonical_repository_root, check_cancellation, project_adapter_supports_language,
    repository_path_hash,
};

const MAX_OBSERVATION_ROWS: usize = 65_536;
const MAX_OBSERVATION_BYTES: usize = 16 * 1024 * 1024;

/// Borrowed production inputs immediately after repository syntax allocation.
pub(super) struct PreflightInput<'a> {
    pub(super) repository: RepositoryId,
    pub(super) generation: GenerationId,
    pub(super) retained_sources: &'a [RustSourceInput],
    pub(super) source_languages: &'a BTreeMap<FileId, String>,
    pub(super) requirements: &'a BTreeMap<FileId, StructuralFactRequirement>,
    pub(super) partitions: &'a BTreeMap<FileId, AnalysisLimits>,
    pub(super) artifacts: &'a StructuralArtifactCollector,
    pub(super) limits: &'a AnalysisLimits,
}

struct CaptureState {
    report: Option<Vec<u8>>,
}

thread_local! {
    static CAPTURE: RefCell<Option<CaptureState>> = const { RefCell::new(None) };
}

#[must_use]
struct CaptureGuard {
    _thread: PhantomData<Rc<()>>,
}

impl CaptureGuard {
    fn arm() -> Result<Self, FirstSliceError> {
        CAPTURE.with(|slot| {
            let mut slot = slot
                .try_borrow_mut()
                .map_err(|_| FirstSliceError::Retention)?;
            if slot.is_some() {
                return Err(FirstSliceError::Identity);
            }
            *slot = Some(CaptureState { report: None });
            Ok(Self {
                _thread: PhantomData,
            })
        })
    }

    fn finish(self) -> Result<Vec<u8>, FirstSliceError> {
        CAPTURE.with(|slot| {
            slot.try_borrow_mut()
                .map_err(|_| FirstSliceError::Retention)?
                .take()
                .and_then(|state| state.report)
                .ok_or(FirstSliceError::Identity)
        })
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        // Destruction must remain safe during unwinding or thread-local teardown.
        let _ = CAPTURE.try_with(|slot| {
            if let Ok(mut slot) = slot.try_borrow_mut() {
                slot.take();
            }
        });
    }
}

#[derive(Serialize)]
struct ObservationRow<'a> {
    path: &'a str,
    file: FileId,
    content_hash: ContentHash,
    source_bytes: usize,
    encoding: &'a str,
    generated: bool,
    routed_language: Option<&'a str>,
    admission_kind: &'static str,
    partition_weight: Option<usize>,
    required_records: Option<usize>,
    complete_records: Option<usize>,
    admitted_records: Option<usize>,
    artifact_retained: bool,
    artifact_records: Option<usize>,
    artifact_syntax_partition_complete: Option<bool>,
    artifact_accounted_bytes: Option<usize>,
}

#[derive(Default, Serialize)]
struct Totals {
    retained_sources: usize,
    source_bytes: usize,
    routed_sources: usize,
    unrouted_sources: usize,
    unrouted_source_bytes: usize,
    required_partitions: usize,
    required_records: usize,
    known_complete_records: usize,
    incomplete_demands: usize,
    admitted_records: usize,
    retained_artifacts: usize,
    complete_demands_above_quota: usize,
    grouped: AdmissionTotals,
    streamed: AdmissionTotals,
}

#[derive(Default, Serialize)]
struct AdmissionTotals {
    sources: usize,
    source_bytes: usize,
    required_records: usize,
    known_complete_records: usize,
    incomplete_demands: usize,
    admitted_records: usize,
    complete_demands_above_quota: usize,
}

impl AdmissionTotals {
    fn observe(
        &mut self,
        source_bytes: usize,
        requirement: &StructuralFactRequirement,
        quota: usize,
    ) -> Result<(), FirstSliceError> {
        add(&mut self.sources, 1)?;
        add(&mut self.source_bytes, source_bytes)?;
        add(&mut self.required_records, requirement.required)?;
        add(&mut self.admitted_records, quota)?;
        if let Some(records) = requirement.complete_records {
            add(&mut self.known_complete_records, records)?;
            if records > quota {
                add(&mut self.complete_demands_above_quota, 1)?;
            }
        } else {
            add(&mut self.incomplete_demands, 1)?;
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct Observation<'a> {
    schema: &'static str,
    repository: RepositoryId,
    generation: GenerationId,
    grouped_record_cap: usize,
    streamed_file_record_cap: usize,
    syntax_stream_record_cap: usize,
    maximum_source_bytes: usize,
    maximum_syntax_nodes: usize,
    maximum_syntax_depth: usize,
    maximum_embedded_ranges: usize,
    maximum_reported_memory_bytes: usize,
    artifact_retention_complete: bool,
    retained_artifact_bytes: usize,
    artifact_retention_byte_cap: usize,
    aborted_before_lowering: bool,
    source_text_included: bool,
    syntax_partition_completeness_is_not_semantic_coverage: bool,
    raw_parse_coverage_observed: bool,
    raw_parse_diagnostic_codes_observed: bool,
    product_success_qualified: bool,
    benchmark_qualified: bool,
    totals: Totals,
    rows: Vec<ObservationRow<'a>>,
}

struct BoundedJson<'a> {
    bytes: Vec<u8>,
    maximum: usize,
    cancellation: &'a Cancellation,
    failure: Option<FirstSliceError>,
}

impl Write for BoundedJson<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = (|| {
            check_cancellation(self.cancellation)?;
            let length = self
                .bytes
                .len()
                .checked_add(bytes.len())
                .ok_or(FirstSliceError::Limits)?;
            if length > self.maximum {
                return Err(super::resource_limit(
                    FirstSliceResource::OutputBytes,
                    length,
                    self.maximum,
                ));
            }
            self.bytes.try_reserve(bytes.len()).map_err(|_| {
                FirstSliceError::ResourceUnavailable {
                    resource: FirstSliceResource::MemoryBytes,
                }
            })?;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        })();
        match result {
            Ok(length) => Ok(length),
            Err(error) => {
                self.failure = Some(error);
                Err(io::Error::other(
                    "syntax admission observation is unavailable",
                ))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn add(total: &mut usize, value: usize) -> Result<(), FirstSliceError> {
    *total = total.checked_add(value).ok_or(FirstSliceError::Limits)?;
    Ok(())
}

fn encode(
    input: PreflightInput<'_>,
    cancellation: &Cancellation,
) -> Result<Vec<u8>, FirstSliceError> {
    check_cancellation(cancellation)?;
    let count = input.retained_sources.len();
    if count > MAX_OBSERVATION_ROWS {
        return Err(super::resource_limit(
            FirstSliceResource::SourceFiles,
            count,
            MAX_OBSERVATION_ROWS,
        ));
    }
    let mut files = HashSet::new();
    files
        .try_reserve(count)
        .map_err(|_| FirstSliceError::ResourceUnavailable {
            resource: FirstSliceResource::MemoryBytes,
        })?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(count)
        .map_err(|_| FirstSliceError::ResourceUnavailable {
            resource: FirstSliceResource::MemoryBytes,
        })?;
    let mut totals = Totals::default();
    let record_cap =
        MAX_FIRST_SLICE_STRUCTURAL_FACTS.min(input.limits.syntax_stream().max_records());
    for source in input.retained_sources {
        check_cancellation(cancellation)?;
        let snapshot = &source.snapshot;
        let file = snapshot.file();
        if !files.insert(file) {
            return Err(FirstSliceError::Identity);
        }
        let language = input.source_languages.get(&file).map(String::as_str);
        let admission_kind = match language {
            Some(language) if project_adapter_supports_language(language) => "grouped_project",
            Some(_) => "streamed_file",
            None => "unrouted",
        };
        let requirement = input.requirements.get(&file);
        let partition = input.partitions.get(&file);
        if requirement.is_some() != partition.is_some()
            || requirement.is_some() != language.is_some()
        {
            return Err(FirstSliceError::Identity);
        }
        let quota = partition.map(|limits| limits.syntax_stream().max_records());
        if let (Some(requirement), Some(quota)) = (requirement, quota) {
            if quota > record_cap
                || quota < requirement.required.max(1)
                || requirement
                    .complete_records
                    .is_some_and(|records| records < requirement.required)
            {
                return Err(FirstSliceError::Identity);
            }
            add(&mut totals.required_partitions, 1)?;
            add(&mut totals.required_records, requirement.required)?;
            add(&mut totals.admitted_records, quota)?;
            if let Some(records) = requirement.complete_records {
                add(&mut totals.known_complete_records, records)?;
                if records > quota {
                    add(&mut totals.complete_demands_above_quota, 1)?;
                }
            } else {
                add(&mut totals.incomplete_demands, 1)?;
            }
            let cohort = if admission_kind == "grouped_project" {
                &mut totals.grouped
            } else {
                &mut totals.streamed
            };
            cohort.observe(snapshot.content().len(), requirement, quota)?;
        }
        let artifact = input.artifacts.get(file).map(|entry| &entry.artifact);
        if let Some(artifact) = artifact {
            if requirement.is_none()
                || artifact.file() != file
                || artifact.content_hash() != snapshot.content_hash()
            {
                return Err(FirstSliceError::Identity);
            }
            if artifact.syntax_partition_complete()
                && requirement.and_then(|requirement| requirement.complete_records)
                    != Some(artifact.syntax_fact_count())
            {
                return Err(FirstSliceError::Identity);
            }
            add(&mut totals.retained_artifacts, 1)?;
        }
        add(&mut totals.retained_sources, 1)?;
        add(&mut totals.source_bytes, snapshot.content().len())?;
        if language.is_some() {
            add(&mut totals.routed_sources, 1)?;
        } else {
            add(&mut totals.unrouted_sources, 1)?;
            add(&mut totals.unrouted_source_bytes, snapshot.content().len())?;
        }
        rows.push(ObservationRow {
            path: snapshot.path().as_str(),
            file,
            content_hash: snapshot.content_hash(),
            source_bytes: snapshot.content().len(),
            encoding: &source.encoding,
            generated: source.generated,
            routed_language: language,
            admission_kind,
            partition_weight: requirement.map(|requirement| requirement.weight),
            required_records: requirement.map(|requirement| requirement.required),
            complete_records: requirement.and_then(|requirement| requirement.complete_records),
            admitted_records: quota,
            artifact_retained: artifact.is_some(),
            artifact_records: artifact.map(|artifact| artifact.syntax_fact_count()),
            artifact_syntax_partition_complete: artifact
                .map(|artifact| artifact.syntax_partition_complete()),
            artifact_accounted_bytes: artifact.map(|artifact| artifact.accounted_bytes()),
        });
    }
    for file in input
        .source_languages
        .keys()
        .chain(input.requirements.keys())
        .chain(input.partitions.keys())
    {
        check_cancellation(cancellation)?;
        if !files.contains(file) {
            return Err(FirstSliceError::Identity);
        }
    }
    // Streamed limits bound one file at a time; adding their quotas is not the
    // shared project-group allowance enforced by the partition allocator.
    if totals.grouped.required_records > record_cap
        || totals.grouped.admitted_records > record_cap
        || totals.required_partitions != input.requirements.len()
        || totals.required_partitions != input.partitions.len()
        || totals.routed_sources != input.source_languages.len()
        || totals.required_partitions != totals.routed_sources
    {
        return Err(FirstSliceError::Identity);
    }
    for (total, grouped, streamed) in [
        (
            totals.routed_sources,
            totals.grouped.sources,
            totals.streamed.sources,
        ),
        (
            totals.required_records,
            totals.grouped.required_records,
            totals.streamed.required_records,
        ),
        (
            totals.known_complete_records,
            totals.grouped.known_complete_records,
            totals.streamed.known_complete_records,
        ),
        (
            totals.incomplete_demands,
            totals.grouped.incomplete_demands,
            totals.streamed.incomplete_demands,
        ),
        (
            totals.admitted_records,
            totals.grouped.admitted_records,
            totals.streamed.admitted_records,
        ),
        (
            totals.complete_demands_above_quota,
            totals.grouped.complete_demands_above_quota,
            totals.streamed.complete_demands_above_quota,
        ),
        (
            totals.retained_sources,
            totals.routed_sources,
            totals.unrouted_sources,
        ),
        (
            totals.source_bytes,
            totals
                .grouped
                .source_bytes
                .checked_add(totals.streamed.source_bytes)
                .ok_or(FirstSliceError::Limits)?,
            totals.unrouted_source_bytes,
        ),
    ] {
        if grouped.checked_add(streamed) != Some(total) {
            return Err(FirstSliceError::Identity);
        }
    }
    let observation = Observation {
        schema: "rootlight.repository-syntax-admission-observation/2",
        repository: input.repository,
        generation: input.generation,
        grouped_record_cap: record_cap,
        streamed_file_record_cap: record_cap,
        syntax_stream_record_cap: input.limits.syntax_stream().max_records(),
        maximum_source_bytes: input.limits.max_source_bytes(),
        maximum_syntax_nodes: input.limits.max_syntax_nodes(),
        maximum_syntax_depth: input.limits.max_syntax_depth(),
        maximum_embedded_ranges: input.limits.max_embedded_ranges(),
        maximum_reported_memory_bytes: input.limits.max_reported_memory_bytes(),
        artifact_retention_complete: input.artifacts.complete,
        retained_artifact_bytes: input.artifacts.accounted_bytes,
        artifact_retention_byte_cap: input.artifacts.maximum_bytes,
        aborted_before_lowering: true,
        source_text_included: false,
        syntax_partition_completeness_is_not_semantic_coverage: true,
        raw_parse_coverage_observed: false,
        raw_parse_diagnostic_codes_observed: false,
        product_success_qualified: false,
        benchmark_qualified: false,
        totals,
        rows,
    };
    let mut output = BoundedJson {
        bytes: Vec::new(),
        maximum: MAX_OBSERVATION_BYTES,
        cancellation,
        failure: None,
    };
    if serde_json::to_writer(&mut output, &observation).is_err() {
        return Err(output.failure.unwrap_or(FirstSliceError::Identity));
    }
    check_cancellation(cancellation)?;
    Ok(output.bytes)
}

/// Captures an armed thread's preflight and aborts before lowering or publication.
///
/// # Errors
/// Returns typed cancellation after capture, or a bounded observation failure.
/// Without an owned capture guard this is an unconditional no-op.
pub(super) fn observe(
    input: PreflightInput<'_>,
    cancellation: &Cancellation,
) -> Result<(), FirstSliceError> {
    let armed = CAPTURE.with(|slot| {
        slot.try_borrow()
            .map(|slot| slot.is_some())
            .map_err(|_| FirstSliceError::Retention)
    })?;
    if !armed {
        return Ok(());
    }
    let report = encode(input, cancellation)?;
    CAPTURE.with(|slot| {
        let mut slot = slot
            .try_borrow_mut()
            .map_err(|_| FirstSliceError::Retention)?;
        let state = slot.as_mut().ok_or(FirstSliceError::Identity)?;
        if state.report.is_some() {
            return Err(FirstSliceError::Identity);
        }
        state.report = Some(report);
        Ok(())
    })?;
    Err(FirstSliceError::Cancelled(
        CancellationReason::ClientRequest,
    ))
}

fn deadline(seconds: u64) -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(seconds))
            .unwrap(),
    )
}

#[test]
fn public_prepare_observation_conserves_demands_without_publication() {
    use rootlight_ids::derive_repository;
    use rootlight_vfs::{RelativePath, RepositoryRoot};

    let root = tempfile::tempdir().unwrap();
    for (path, text) in [
        ("Alpha.rs", "pub fn identity(value: u32) -> u32 { value }\n"),
        (
            "Beta.rs",
            "pub fn doubled(value: u32) -> u32 { value + value }\n",
        ),
        ("Notes.data", "plain retained input\n"),
    ] {
        std::fs::write(root.path().join(path), text).unwrap();
    }
    let cancellation = deadline(30);
    let service = FirstSliceService::new(1).unwrap();
    let repository = derive_repository(b"syntax-admission-observer-contract").id();
    let canonical = canonical_repository_root(root.path(), &cancellation).unwrap();
    service
        .restore_repository_registration(repository_path_hash(&canonical).unwrap(), repository)
        .unwrap();
    let original_limits = service.analysis_limits.clone();
    let guard = CaptureGuard::arm().unwrap();
    assert!(matches!(
        service.prepare_repository(root.path(), &cancellation),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    let bytes = guard.finish().unwrap();
    assert!(bytes.len() <= MAX_OBSERVATION_BYTES);
    let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        report["schema"],
        "rootlight.repository-syntax-admission-observation/2"
    );
    assert_eq!(
        report["repository"],
        serde_json::to_value(repository).unwrap()
    );
    assert_eq!(report["aborted_before_lowering"], true);
    assert_eq!(report["product_success_qualified"], false);
    assert_eq!(service.analysis_limits, original_limits);
    assert_eq!(report["totals"]["retained_sources"], 3);
    assert_eq!(report["totals"]["required_partitions"], 2);
    assert_eq!(report["totals"]["grouped"]["sources"], 2);
    assert_eq!(report["totals"]["streamed"]["sources"], 0);
    assert_eq!(report["totals"]["unrouted_sources"], 1);
    assert_eq!(report["totals"]["incomplete_demands"], 0);
    assert_eq!(report["totals"]["complete_demands_above_quota"], 0);
    assert!(
        report["totals"]["admitted_records"].as_u64().unwrap()
            <= report["grouped_record_cap"].as_u64().unwrap()
    );
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let capability = RepositoryRoot::open(repository, &canonical).unwrap();
    for path in ["Alpha.rs", "Beta.rs", "Notes.data"] {
        let matches = rows
            .iter()
            .filter(|row| row["path"] == path)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{path}");
        let row = matches[0];
        let relative = RelativePath::parse(std::path::Path::new(path)).unwrap();
        let snapshot = capability
            .snapshot(
                &relative,
                u64::try_from(original_limits.max_source_bytes()).unwrap(),
            )
            .unwrap();
        assert_eq!(row["file"], serde_json::to_value(snapshot.file()).unwrap());
        assert_eq!(
            row["content_hash"],
            serde_json::to_value(snapshot.content_hash()).unwrap()
        );
        assert_eq!(
            row["source_bytes"].as_u64(),
            Some(u64::try_from(snapshot.content().len()).unwrap())
        );
        assert_eq!(row["encoding"], "utf-8");
        assert_eq!(row["generated"], false);
        if path.ends_with(".rs") {
            assert_eq!(row["admission_kind"], "grouped_project");
            assert_eq!(row["routed_language"], "rust");
            let required = row["required_records"].as_u64().unwrap();
            let complete = row["complete_records"].as_u64().unwrap();
            assert!(required > 0 && complete >= required, "{path}");
            assert_eq!(row["admitted_records"].as_u64(), Some(complete));
            assert_eq!(row["artifact_records"].as_u64(), Some(complete));
            assert_eq!(row["artifact_retained"], true);
            assert_eq!(row["artifact_syntax_partition_complete"], true);
        } else {
            assert_eq!(row["admission_kind"], "unrouted");
            assert!(row["routed_language"].is_null());
            assert!(row["required_records"].is_null());
            assert!(row["admitted_records"].is_null());
            assert_eq!(row["artifact_retained"], false);
        }
    }
    assert!(service.active_generation().is_none());
    assert!(service.active_generation_for(repository).is_none());
    assert!(service.receipts.is_empty());
    assert!(service.active_by_repository.is_empty());
    assert!(service.source_snapshots.committed.is_empty());
    assert!(service.source_snapshots.staged.is_empty());
    assert!(service.structural_artifacts.committed.is_empty());
    assert!(service.structural_artifacts.staged.is_empty());
    assert!(service.registered_repository_roots().unwrap().is_empty());
    assert!(CAPTURE.with(|slot| slot.borrow().is_none()));
    let prepared = service
        .prepare_repository(root.path(), &cancellation)
        .unwrap();
    assert!(matches!(
        prepared,
        super::FirstSliceIndexPreparation::Pending(_)
    ));
    assert!(CAPTURE.with(|slot| slot.borrow().is_none()));
    assert!(service.active_generation().is_none());
}

#[test]
fn mixed_prepare_observation_separates_shared_and_per_file_quotas() {
    let root = tempfile::tempdir().unwrap();
    let inputs = [
        ("Anchor.rs", "pub fn anchor(value: u32) -> u32 { value }\n"),
        ("Alpha.fs", "module Alpha\nlet identity value = value\n"),
        (
            "Beta.fs",
            "module Beta\nlet doubled value = value + value\n",
        ),
    ];
    for (path, text) in inputs {
        std::fs::write(root.path().join(path), text).unwrap();
    }
    let cancellation = deadline(30);
    let mut service = FirstSliceService::new(1).unwrap();
    service.analysis_limits =
        super::analysis_limits_with_syntax_records(&service.analysis_limits, 128).unwrap();
    let guard = CaptureGuard::arm().unwrap();
    assert!(matches!(
        service.prepare_repository(root.path(), &cancellation),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    let bytes = guard.finish().unwrap();
    let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        report["schema"],
        "rootlight.repository-syntax-admission-observation/2"
    );
    assert_eq!(report["grouped_record_cap"], 128);
    assert_eq!(report["streamed_file_record_cap"], 128);
    assert_eq!(report["totals"]["retained_sources"], 3);
    assert_eq!(report["totals"]["routed_sources"], 3);
    assert_eq!(report["totals"]["required_partitions"], 3);
    assert_eq!(report["totals"]["grouped"]["sources"], 1);
    assert_eq!(report["totals"]["streamed"]["sources"], 2);
    assert_eq!(report["totals"]["unrouted_sources"], 0);
    assert_eq!(report["totals"]["incomplete_demands"], 0);
    assert_eq!(report["totals"]["complete_demands_above_quota"], 0);
    let grouped = report["totals"]["grouped"]["admitted_records"]
        .as_u64()
        .unwrap();
    let streamed = report["totals"]["streamed"]["admitted_records"]
        .as_u64()
        .unwrap();
    assert!(grouped > 0 && grouped <= 128);
    assert_eq!(streamed, 256);
    assert_eq!(
        report["totals"]["admitted_records"].as_u64(),
        Some(grouped + streamed)
    );
    assert!(report["totals"]["admitted_records"].as_u64().unwrap() > 128);
    for field in [
        "required_records",
        "known_complete_records",
        "incomplete_demands",
        "complete_demands_above_quota",
    ] {
        assert_eq!(
            report["totals"][field].as_u64().unwrap(),
            report["totals"]["grouped"][field].as_u64().unwrap()
                + report["totals"]["streamed"][field].as_u64().unwrap(),
            "{field}"
        );
    }
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.len(), inputs.len());
    let mut grouped_bytes = 0_u64;
    let mut streamed_bytes = 0_u64;
    for (path, text) in inputs {
        let matches = rows
            .iter()
            .filter(|row| row["path"] == path)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{path}");
        let row = matches[0];
        let source_bytes = u64::try_from(text.len()).unwrap();
        assert_eq!(row["source_bytes"].as_u64(), Some(source_bytes));
        assert_eq!(
            row["content_hash"],
            serde_json::to_value(rootlight_ids::content_hash(text.as_bytes())).unwrap()
        );
        assert_eq!(row["encoding"], "utf-8");
        assert_eq!(row["generated"], false);
        let required = row["required_records"].as_u64().unwrap();
        let complete = row["complete_records"].as_u64().unwrap();
        assert!(required > 0 && required <= complete && complete <= 128);
        if path.ends_with(".rs") {
            assert_eq!(row["routed_language"], "rust");
            assert_eq!(row["admission_kind"], "grouped_project");
            assert_eq!(row["admitted_records"].as_u64(), Some(complete));
            grouped_bytes += source_bytes;
        } else {
            assert_eq!(row["routed_language"], "fsharp");
            assert_eq!(row["admission_kind"], "streamed_file");
            assert_eq!(row["admitted_records"], 128);
            streamed_bytes += source_bytes;
        }
    }
    assert_eq!(
        report["totals"]["grouped"]["source_bytes"].as_u64(),
        Some(grouped_bytes)
    );
    assert_eq!(
        report["totals"]["streamed"]["source_bytes"].as_u64(),
        Some(streamed_bytes)
    );
    assert_eq!(
        report["totals"]["source_bytes"].as_u64(),
        Some(grouped_bytes + streamed_bytes)
    );
    assert_eq!(report["source_text_included"], false);
    assert_eq!(report["aborted_before_lowering"], true);
    assert_eq!(report["product_success_qualified"], false);
    assert_eq!(report["benchmark_qualified"], false);
    assert!(service.active_generation().is_none());
    assert!(service.receipts.is_empty());
    assert!(service.source_snapshots.committed.is_empty());
    assert!(service.source_snapshots.staged.is_empty());
    assert!(service.structural_artifacts.committed.is_empty());
    assert!(service.structural_artifacts.staged.is_empty());
    assert!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reservations
            .is_empty()
    );
    assert!(CAPTURE.with(|slot| slot.borrow().is_none()));
}

#[test]
fn capture_guard_drop_and_nested_rejection_leave_normal_threads_unarmed() {
    let guard = CaptureGuard::arm().unwrap();
    assert!(matches!(
        CaptureGuard::arm(),
        Err(FirstSliceError::Identity)
    ));
    assert!(
        std::thread::spawn(|| CAPTURE.with(|slot| slot.borrow().is_none()))
            .join()
            .unwrap()
    );
    drop(guard);
    assert!(CAPTURE.with(|slot| slot.borrow().is_none()));
    assert!(CaptureGuard::arm().unwrap().finish().is_err());
    assert!(CAPTURE.with(|slot| slot.borrow().is_none()));
}

#[test]
fn observation_json_writer_rejects_growth_without_partial_success() {
    let cancellation = deadline(30);
    let mut output = BoundedJson {
        bytes: Vec::new(),
        maximum: 8,
        cancellation: &cancellation,
        failure: None,
    };
    output.write_all(b"12345678").unwrap();
    assert!(output.write_all(b"9").is_err());
    assert_eq!(output.bytes, b"12345678");
    assert!(matches!(
        output.failure,
        Some(FirstSliceError::ResourceLimit {
            resource: FirstSliceResource::OutputBytes,
            observed: 9,
            limit: 8
        })
    ));
}

#[test]
#[ignore = "requires an explicit repository, identity and create-new diagnostic output"]
fn observe_repository_syntax_admission() {
    let root = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_ADMISSION_ROOT").expect("explicit repository root is required"),
    );
    let output = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_ADMISSION_OUTPUT")
            .expect("explicit diagnostic output is required"),
    );
    let repository: RepositoryId = std::env::var("ROOTLIGHT_ADMISSION_REPOSITORY")
        .expect("explicit repository identity is required")
        .parse()
        .expect("repository identity is valid");
    let cancellation = deadline(3600);
    let canonical = canonical_repository_root(&root, &cancellation).unwrap();
    let output = std::path::absolute(output).unwrap();
    let parent = std::fs::canonicalize(output.parent().expect("output has a parent")).unwrap();
    assert!(
        !parent.starts_with(&canonical),
        "diagnostic output must be outside the observed repository"
    );
    let output = parent.join(output.file_name().expect("output has a file name"));
    assert!(!output.exists(), "diagnostic output must be fresh");
    let service = FirstSliceService::new(1).unwrap();
    service
        .restore_repository_registration(repository_path_hash(&canonical).unwrap(), repository)
        .unwrap();
    let guard = CaptureGuard::arm().unwrap();
    let result = service.prepare_repository(&canonical, &cancellation);
    assert!(
        matches!(
            result,
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ),
        "public preparation must stop at the captured boundary"
    );
    let report = guard.finish().unwrap();
    assert!(report.len() <= MAX_OBSERVATION_BYTES);
    assert!(service.active_generation().is_none());
    assert!(service.active_generation_for(repository).is_none());
    assert!(service.receipts.is_empty());
    assert!(service.active_by_repository.is_empty());
    assert!(service.registered_repository_roots().unwrap().is_empty());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .unwrap();
    file.write_all(&report).unwrap();
    file.sync_all().unwrap();
}
