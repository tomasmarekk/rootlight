//! Test-only, armed-thread cost accounting for one generation preparation.
//! Fixed numeric aggregates describe conservative allowances, not owned memory.
//! Only a caller-classified propagated failure seals the source-free observation.

use std::{
    cell::RefCell,
    io::{self, Write},
    marker::PhantomData,
    rc::Rc,
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use serde::Serialize;

use super::{FirstSliceError, FirstSliceResource, check_cancellation, resource_limit};
use crate::generation_text_observation::{self, TextObservation};

/// Maximum encoded size of this fixed-shape diagnostic.
pub(super) const MAX_OBSERVATION_BYTES: usize = 16 * 1024;

/// Classification supplied by the caller, never inferred from an error's size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    #[default]
    Unclassified,
    SourcePreflight,
    DispositionDocument,
    FileDocument,
    ProjectDocument,
    OptionalChunk,
    FinalProjection,
    FinalVerification,
}

#[derive(Clone, Copy, Default)]
enum ChargeKind {
    Document,
    #[default]
    KnownBytes,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
struct Usage {
    count: u64,
    bytes: u64,
}

impl Usage {
    fn added(self, bytes: u64) -> Result<Self, FirstSliceError> {
        Ok(Self {
            count: self.count.checked_add(1).ok_or(FirstSliceError::Limits)?,
            bytes: self
                .bytes
                .checked_add(bytes)
                .ok_or(FirstSliceError::Limits)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCode {
    GenerationMemoryLimit,
    ResourceLimit,
    Cancelled,
    Limits,
    Other,
}

#[derive(Clone, Copy, Serialize)]
struct MemoryLimit {
    retained_bytes: u64,
    reserved_bytes: u64,
    owned_bytes: u64,
    referenced_bytes: u64,
    mapped_bytes: u64,
    staged_bytes: u64,
    shared_bytes: u64,
    observed: u64,
    limit: u64,
}

#[derive(Clone, Copy, Serialize)]
struct ResourceLimit {
    resource: &'static str,
    observed: u64,
    limit: u64,
}

#[derive(Clone, Copy, Serialize)]
struct ErrorSummary {
    code: ErrorCode,
    memory_limit: Option<MemoryLimit>,
    resource_limit: Option<ResourceLimit>,
}

impl ErrorSummary {
    fn new(error: &FirstSliceError) -> Self {
        let (code, memory_limit) = match *error {
            FirstSliceError::GenerationMemoryLimit {
                breakdown,
                observed,
                limit,
            } => (
                ErrorCode::GenerationMemoryLimit,
                Some(MemoryLimit {
                    retained_bytes: breakdown.retained_bytes,
                    reserved_bytes: breakdown.reserved_bytes,
                    owned_bytes: breakdown.owned_bytes,
                    referenced_bytes: breakdown.referenced_bytes,
                    mapped_bytes: breakdown.mapped_bytes,
                    staged_bytes: breakdown.staged_bytes,
                    shared_bytes: breakdown.shared_bytes,
                    observed,
                    limit,
                }),
            ),
            FirstSliceError::Cancelled(_) => (ErrorCode::Cancelled, None),
            FirstSliceError::Limits => (ErrorCode::Limits, None),
            _ => (ErrorCode::Other, None),
        };
        let resource_limit = if let FirstSliceError::ResourceLimit {
            resource,
            observed,
            limit,
        } = *error
        {
            Some(ResourceLimit {
                resource: resource.as_str(),
                observed,
                limit,
            })
        } else {
            None
        };
        Self {
            code: if resource_limit.is_some() {
                ErrorCode::ResourceLimit
            } else {
                code
            },
            memory_limit,
            resource_limit,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
struct SerializationAttempt {
    phase: Phase,
    admitted_base_bytes: u64,
    accepted_prefix_bytes: u64,
    attempted_prefix_bytes: Option<u64>,
    rejected_write_bytes: Option<u64>,
    complete_bytes: Option<u64>,
    error: Option<ErrorCode>,
}

/// Components of one optional construction peak, including rejected attempts.
#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct TemporaryPeak {
    /// Existing monotonic reservation before optional work.
    pub(super) base_bytes: u64,
    /// Exact serialized document size used by the four-document allowance.
    pub(super) document_bytes: u64,
    /// Compact graph workspace allowance.
    pub(super) workspace_bytes: u64,
    /// Encoded-chunk descriptor allowance.
    pub(super) descriptor_bytes: u64,
    /// Four documents plus workspace and descriptor.
    pub(super) work_bytes: u64,
    /// Existing base plus work allowance.
    pub(super) peak_bytes: u64,
    /// Whether temporary reservation growth actually succeeded.
    pub(super) admitted: bool,
}

impl TemporaryPeak {
    fn validate(self) -> Result<(), FirstSliceError> {
        let work = self
            .document_bytes
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(self.workspace_bytes))
            .and_then(|bytes| bytes.checked_add(self.descriptor_bytes))
            .ok_or(FirstSliceError::Limits)?;
        let peak = self
            .base_bytes
            .checked_add(work)
            .ok_or(FirstSliceError::Limits)?;
        if work != self.work_bytes || peak != self.peak_bytes {
            return Err(FirstSliceError::Identity);
        }
        Ok(())
    }
}

/// Components supplied at final projection admission, not reconstructed later.
#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct ProjectionPeak {
    /// Exact complete normalized-document serialization size.
    pub(super) document_bytes: u64,
    /// Additional source lexical-coverage allowance.
    pub(super) lexical_coverage_bytes: u64,
    /// Compact graph workspace allowance.
    pub(super) workspace_bytes: u64,
    /// Ordinary snapshot metadata retained for authenticated source recovery.
    pub(super) snapshot_metadata_bytes: u64,
    /// Currently retained structural-cache allowance.
    pub(super) structural_cache_bytes: u64,
    /// Explain-index construction peak allowance.
    pub(super) explain_index_bytes: u64,
    /// Three document/lexical allowances plus the remaining components.
    pub(super) peak_bytes: u64,
}

#[derive(Clone, Copy, Serialize)]
struct TemporaryReservation {
    base_bytes: u64,
    work_bytes: u64,
    peak_bytes: u64,
    admitted: bool,
}

impl ProjectionPeak {
    fn validate(self) -> Result<(), FirstSliceError> {
        let peak = self
            .document_bytes
            .checked_add(self.lexical_coverage_bytes)
            .and_then(|bytes| bytes.checked_mul(3))
            .and_then(|bytes| bytes.checked_add(self.workspace_bytes))
            .and_then(|bytes| bytes.checked_add(self.snapshot_metadata_bytes))
            .and_then(|bytes| bytes.checked_add(self.structural_cache_bytes))
            .and_then(|bytes| bytes.checked_add(self.explain_index_bytes))
            .ok_or(FirstSliceError::Limits)?;
        if peak != self.peak_bytes {
            return Err(FirstSliceError::Identity);
        }
        Ok(())
    }
}

#[derive(Default, Serialize)]
struct Costs {
    source_floor_bytes: Option<u64>,
    admission_started: bool,
    admitted_documents: Usage,
    admitted_known_bytes: Usage,
    admitted_known_chunks: Usage,
    admitted_total_bytes: Option<u64>,
    last_serialization: Option<SerializationAttempt>,
    attempted_known_bytes: Option<u64>,
    temporary_attempt_count: u64,
    temporary_admitted_count: u64,
    largest_temporary_attempt: Option<TemporaryPeak>,
    largest_admitted_temporary_peak: Option<TemporaryPeak>,
    projection_peak: Option<ProjectionPeak>,
    temporary_reservation_attempt_count: u64,
    temporary_reservation_admitted_count: u64,
    largest_temporary_reservation: Option<TemporaryReservation>,
    text_census: Option<TextObservation>,
}

struct CaptureState {
    id: u64,
    maximum_bytes: usize,
    phase: Phase,
    charge_kind: ChargeKind,
    costs: Costs,
    failure: Option<(Phase, ErrorSummary)>,
    diagnostic_error: Option<FirstSliceError>,
}

thread_local! {
    static CAPTURE: RefCell<Option<CaptureState>> = const { RefCell::new(None) };
    static CAPTURE_LOST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static NEXT_CAPTURE_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn update(work: impl FnOnce(&mut CaptureState) -> Result<(), FirstSliceError>) {
    let _ = CAPTURE.try_with(|slot| {
        let Ok(mut slot) = slot.try_borrow_mut() else {
            let _ = CAPTURE_LOST.try_with(|lost| lost.set(true));
            return;
        };
        if let Some(state) = slot.as_mut()
            && state.failure.is_none()
            && state.diagnostic_error.is_none()
            && let Err(error) = work(state)
        {
            state.diagnostic_error = Some(error);
        }
    });
}

/// Owns an opt-in capture on this thread; nested or multi-admission use is rejected.
#[must_use]
pub(super) struct CaptureGuard {
    id: u64,
    _thread: PhantomData<Rc<()>>,
}

impl CaptureGuard {
    /// Arms a bounded, single-preparation capture without inspecting any inputs.
    ///
    /// # Errors
    /// Rejects nested captures, excessive output limits or unavailable local state.
    pub(super) fn arm(maximum_bytes: usize) -> Result<Self, FirstSliceError> {
        CAPTURE.with(|slot| {
            let mut slot = slot
                .try_borrow_mut()
                .map_err(|_| FirstSliceError::Retention)?;
            if slot.is_some() || maximum_bytes > MAX_OBSERVATION_BYTES {
                return Err(FirstSliceError::Identity);
            }
            let id = NEXT_CAPTURE_ID.with(|next| -> Result<u64, FirstSliceError> {
                let id = next.get().checked_add(1).ok_or(FirstSliceError::Limits)?;
                next.set(id);
                Ok(id)
            })?;
            CAPTURE_LOST.with(|lost| lost.set(false));
            *slot = Some(CaptureState {
                id,
                maximum_bytes,
                phase: Phase::Unclassified,
                charge_kind: ChargeKind::KnownBytes,
                costs: Costs::default(),
                failure: None,
                diagnostic_error: None,
            });
            Ok(Self {
                id,
                _thread: PhantomData,
            })
        })
    }

    /// Indicates only whether the caller recorded a terminal failure boundary.
    pub(super) fn reached(&self) -> bool {
        CAPTURE.with(|slot| {
            slot.try_borrow().is_ok_and(|slot| {
                slot.as_ref()
                    .is_some_and(|state| state.id == self.id && state.failure.is_some())
            })
        })
    }

    /// Encodes the sealed numeric report; never returns a replacement service error.
    ///
    /// # Errors
    /// Returns capture, arithmetic, conservation, cancellation or output failures.
    pub(super) fn finish(self, cancellation: &Cancellation) -> Result<Vec<u8>, FirstSliceError> {
        let state = CAPTURE.with(|slot| {
            let mut slot = slot
                .try_borrow_mut()
                .map_err(|_| FirstSliceError::Retention)?;
            if slot.as_ref().is_none_or(|state| state.id != self.id) {
                return Err(FirstSliceError::Identity);
            }
            slot.take().ok_or(FirstSliceError::Identity)
        })?;
        if CAPTURE_LOST.with(std::cell::Cell::get) {
            return Err(FirstSliceError::Retention);
        }
        if let Some(error) = state.diagnostic_error {
            return Err(error);
        }
        let (failure_phase, original_error) = state.failure.ok_or(FirstSliceError::Identity)?;
        let report = Observation {
            schema: "rootlight.failed-generation-admission-observation/1",
            scope: "armed_thread_one_generation_preparation",
            failure_phase,
            failure_phase_classified_by_caller: failure_phase != Phase::Unclassified,
            original_error,
            costs: state.costs,
            source_text_included: false,
            payload_contents_included: false,
            owned_heap_measured: false,
            process_rss_measured: false,
            process_wide_census: false,
            whole_repository_observed: false,
            product_success_qualified: false,
            benchmark_qualified: false,
        };
        encode(&report, state.maximum_bytes, cancellation)
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        // Teardown must not panic while unwinding or destroying thread-local state.
        let _ = CAPTURE.try_with(|slot| {
            if let Ok(mut slot) = slot.try_borrow_mut()
                && slot.as_ref().is_some_and(|state| state.id == self.id)
            {
                slot.take();
            }
        });
    }
}

/// Restores caller classification across early return or unwinding.
#[must_use]
pub(super) struct PhaseGuard {
    previous: Option<(u64, Phase)>,
    _thread: PhantomData<Rc<()>>,
}

/// Temporarily classifies armed-thread work; unarmed work remains unchanged.
pub(super) fn enter_phase(phase: Phase) -> PhaseGuard {
    let mut previous = None;
    update(|state| {
        previous = Some((state.id, state.phase));
        state.phase = phase;
        Ok(())
    });
    PhaseGuard {
        previous,
        _thread: PhantomData,
    }
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        if let Some((id, previous)) = self.previous {
            // Restoration also runs after sealing; the sealed phase is a copy.
            let _ = CAPTURE.try_with(|slot| {
                if let Ok(mut slot) = slot.try_borrow_mut()
                    && let Some(state) = slot.as_mut()
                    && state.id == id
                {
                    state.phase = previous;
                }
            });
        }
    }
}

/// Keeps serialized-document charging distinct within the current armed capture.
#[must_use]
pub(super) struct DocumentChargeGuard {
    previous: Option<(u64, ChargeKind)>,
    _thread: PhantomData<Rc<()>>,
}

/// Prevents a serialized document's internal known-byte admission counting as a chunk.
pub(super) fn enter_document_charge() -> DocumentChargeGuard {
    let mut previous = None;
    update(|state| {
        previous = Some((state.id, state.charge_kind));
        state.charge_kind = ChargeKind::Document;
        Ok(())
    });
    DocumentChargeGuard {
        previous,
        _thread: PhantomData,
    }
}

impl Drop for DocumentChargeGuard {
    fn drop(&mut self) {
        if let Some((id, previous)) = self.previous {
            let _ = CAPTURE.try_with(|slot| {
                if let Ok(mut slot) = slot.try_borrow_mut()
                    && let Some(state) = slot.as_mut()
                    && state.id == id
                {
                    state.charge_kind = previous;
                }
            });
        }
    }
}

/// Records the exact preflight allowance, including failures before ledger creation.
pub(super) fn observe_source_floor(bytes: u64) {
    update(|state| {
        if state
            .costs
            .source_floor_bytes
            .is_some_and(|prior| prior != bytes)
        {
            return Err(FirstSliceError::Identity);
        }
        state.costs.source_floor_bytes = Some(bytes);
        state.costs.admitted_total_bytes.get_or_insert(bytes);
        Ok(())
    });
}

/// Associates exactly one admission ledger with this armed-thread capture.
pub(super) fn begin_admission(bytes: u64) {
    observe_source_floor(bytes);
    update(|state| {
        if state.costs.admission_started {
            return Err(FirstSliceError::Identity);
        }
        state.costs.admission_started = true;
        Ok(())
    });
}

/// Records requested known bytes without assuming an unclassified caller is a chunk.
pub(super) fn observe_known_attempt(bytes: u64) {
    update(|state| {
        state.costs.attempted_known_bytes = match state.charge_kind {
            ChargeKind::Document => None,
            ChargeKind::KnownBytes => Some(bytes),
        };
        Ok(())
    });
}

/// Commits exact successful charges only after the real reservation has grown.
pub(super) fn observe_admitted(bytes: u64, reservation_bytes: u64) {
    update(|state| {
        let mut documents = state.costs.admitted_documents;
        let mut known = state.costs.admitted_known_bytes;
        let mut chunks = state.costs.admitted_known_chunks;
        match state.charge_kind {
            ChargeKind::Document => documents = documents.added(bytes)?,
            ChargeKind::KnownBytes => {
                known = known.added(bytes)?;
                if state.phase == Phase::OptionalChunk {
                    chunks = chunks.added(bytes)?;
                }
            }
        }
        let total = state
            .costs
            .source_floor_bytes
            .ok_or(FirstSliceError::Identity)?
            .checked_add(documents.bytes)
            .and_then(|bytes| bytes.checked_add(known.bytes))
            .ok_or(FirstSliceError::Limits)?;
        if total != reservation_bytes {
            return Err(FirstSliceError::Identity);
        }
        state.costs.admitted_documents = documents;
        state.costs.admitted_known_bytes = known;
        state.costs.admitted_known_chunks = chunks;
        state.costs.admitted_total_bytes = Some(total);
        Ok(())
    });
}

/// Starts one serializer-prefix observation at the exact admitted ledger base.
pub(super) fn serialization_start(admitted_bytes: u64) {
    update(|state| {
        if state.costs.admitted_total_bytes != Some(admitted_bytes) {
            return Err(FirstSliceError::Identity);
        }
        state.costs.attempted_known_bytes = None;
        state.costs.last_serialization = Some(SerializationAttempt {
            phase: state.phase,
            admitted_base_bytes: admitted_bytes,
            accepted_prefix_bytes: 0,
            attempted_prefix_bytes: None,
            rejected_write_bytes: None,
            complete_bytes: None,
            error: None,
        });
        Ok(())
    });
}

/// Advances only the serializer's accepted prefix, not the admission totals.
pub(super) fn serialization_prefix(bytes: u64) {
    update(|state| {
        let attempt = state
            .costs
            .last_serialization
            .as_mut()
            .ok_or(FirstSliceError::Identity)?;
        if bytes < attempt.accepted_prefix_bytes
            || attempt.complete_bytes.is_some()
            || attempt.error.is_some()
            || attempt.attempted_prefix_bytes.is_some()
        {
            return Err(FirstSliceError::Identity);
        }
        attempt.accepted_prefix_bytes = bytes;
        Ok(())
    });
}

/// Records a rejected write candidate; arithmetic overflow remains explicitly absent.
pub(super) fn serialization_rejected(attempted_prefix: Option<u64>, write_bytes: Option<u64>) {
    update(|state| {
        let attempt = state
            .costs
            .last_serialization
            .as_mut()
            .ok_or(FirstSliceError::Identity)?;
        if let (Some(prefix), Some(write)) = (attempted_prefix, write_bytes)
            && attempt.accepted_prefix_bytes.checked_add(write) != Some(prefix)
        {
            return Err(FirstSliceError::Identity);
        }
        if attempted_prefix.is_none()
            && write_bytes
                .is_some_and(|write| attempt.accepted_prefix_bytes.checked_add(write).is_some())
        {
            return Err(FirstSliceError::Identity);
        }
        attempt.attempted_prefix_bytes = attempted_prefix;
        attempt.rejected_write_bytes = write_bytes;
        Ok(())
    });
}

/// Records a serializer's safe failure category without declaring terminal failure.
pub(super) fn serialization_failed(error: &FirstSliceError) {
    update(|state| {
        let attempt = state
            .costs
            .last_serialization
            .as_mut()
            .ok_or(FirstSliceError::Identity)?;
        attempt.error = Some(ErrorSummary::new(error).code);
        Ok(())
    });
}

/// Marks complete serialization separately from successful reservation admission.
pub(super) fn serialization_complete(bytes: u64) {
    update(|state| {
        let attempt = state
            .costs
            .last_serialization
            .as_mut()
            .ok_or(FirstSliceError::Identity)?;
        if attempt.accepted_prefix_bytes != bytes
            || attempt.error.is_some()
            || attempt.attempted_prefix_bytes.is_some()
            || attempt.rejected_write_bytes.is_some()
        {
            return Err(FirstSliceError::Identity);
        }
        attempt.complete_bytes = Some(bytes);
        Ok(())
    });
}

/// Records conserved optional components without sealing swallowed optional errors.
pub(super) fn observe_temporary_peak(peak: TemporaryPeak) {
    update(|state| {
        peak.validate()?;
        let attempts = state
            .costs
            .temporary_attempt_count
            .checked_add(1)
            .ok_or(FirstSliceError::Limits)?;
        let admitted = state
            .costs
            .temporary_admitted_count
            .checked_add(u64::from(peak.admitted))
            .ok_or(FirstSliceError::Limits)?;
        state.costs.temporary_attempt_count = attempts;
        state.costs.temporary_admitted_count = admitted;
        if state
            .costs
            .largest_temporary_attempt
            .is_none_or(|prior| peak.peak_bytes > prior.peak_bytes)
        {
            state.costs.largest_temporary_attempt = Some(peak);
        }
        if peak.admitted
            && state
                .costs
                .largest_admitted_temporary_peak
                .is_none_or(|prior| peak.peak_bytes > prior.peak_bytes)
        {
            state.costs.largest_admitted_temporary_peak = Some(peak);
        }
        Ok(())
    });
}

/// Records conserved finalization components without changing reservation behavior.
pub(super) fn observe_projection_peak(peak: ProjectionPeak) {
    update(|state| {
        peak.validate()?;
        state.costs.projection_peak = Some(peak);
        Ok(())
    });
}

/// Records exact temporary growth; work bytes remain an allowance, not owned memory.
pub(super) fn observe_temporary_reservation(base_bytes: u64, peak_bytes: u64, admitted: bool) {
    update(|state| {
        let work_bytes = peak_bytes
            .checked_sub(base_bytes)
            .ok_or(FirstSliceError::Identity)?;
        let attempts = state
            .costs
            .temporary_reservation_attempt_count
            .checked_add(1)
            .ok_or(FirstSliceError::Limits)?;
        let admitted_count = state
            .costs
            .temporary_reservation_admitted_count
            .checked_add(u64::from(admitted))
            .ok_or(FirstSliceError::Limits)?;
        state.costs.temporary_reservation_attempt_count = attempts;
        state.costs.temporary_reservation_admitted_count = admitted_count;
        if state
            .costs
            .largest_temporary_reservation
            .is_none_or(|prior| peak_bytes > prior.peak_bytes)
        {
            state.costs.largest_temporary_reservation = Some(TemporaryReservation {
                base_bytes,
                work_bytes,
                peak_bytes,
                admitted,
            });
        }
        Ok(())
    });
}

/// Counts the verification input only for an armed capture, without changing admission.
pub(super) fn observe_text_document(
    document: &rootlight_ir::NormalizedIrDocument,
    source_files: &rootlight_storage::SourceFileCatalog,
    limit: u64,
    cancellation: &Cancellation,
) {
    update(|state| {
        if state.costs.text_census.is_some() {
            return Err(FirstSliceError::Identity);
        }
        state.costs.text_census = Some(generation_text_observation::measure(
            document,
            source_files,
            limit,
            cancellation,
        )?);
        Ok(())
    });
}

/// Seals only the caller's propagated failure; never substitutes a diagnostic error.
pub(super) fn observe_failure(error: &FirstSliceError) {
    let _ = CAPTURE.try_with(|slot| {
        let Ok(mut slot) = slot.try_borrow_mut() else {
            let _ = CAPTURE_LOST.try_with(|lost| lost.set(true));
            return;
        };
        if let Some(state) = slot.as_mut()
            && state.failure.is_none()
        {
            if let FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::TextBytes,
                observed,
                limit,
            } = *error
                && let Some(census) = state.costs.text_census.take()
            {
                match census.reconcile_failure(FirstSliceResource::TextBytes, observed, limit) {
                    Ok(census) => state.costs.text_census = Some(census),
                    Err(error) => {
                        state.diagnostic_error.get_or_insert(error);
                    }
                }
            }
            state.failure = Some((state.phase, ErrorSummary::new(error)));
        }
    });
}

#[derive(Serialize)]
struct Observation {
    schema: &'static str,
    scope: &'static str,
    failure_phase: Phase,
    failure_phase_classified_by_caller: bool,
    original_error: ErrorSummary,
    costs: Costs,
    source_text_included: bool,
    payload_contents_included: bool,
    owned_heap_measured: bool,
    process_rss_measured: bool,
    process_wide_census: bool,
    whole_repository_observed: bool,
    product_success_qualified: bool,
    benchmark_qualified: bool,
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
            let size = self
                .bytes
                .len()
                .checked_add(bytes.len())
                .ok_or(FirstSliceError::Limits)?;
            if size > self.maximum {
                return Err(resource_limit(
                    FirstSliceResource::OutputBytes,
                    size,
                    self.maximum,
                ));
            }
            self.bytes
                .try_reserve(bytes.len())
                .map_err(|_| FirstSliceError::Retention)?;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        })();
        match result {
            Ok(bytes) => Ok(bytes),
            Err(error) => {
                self.failure = Some(error);
                Err(io::Error::other(
                    "generation admission observation is unavailable",
                ))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode(
    value: &impl Serialize,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<Vec<u8>, FirstSliceError> {
    let mut writer = BoundedJson {
        bytes: Vec::new(),
        maximum,
        cancellation,
        failure: None,
    };
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(writer.failure.unwrap_or(FirstSliceError::Identity));
    }
    check_cancellation(cancellation)?;
    Ok(writer.bytes)
}

fn census_cancellation(timeout_seconds: u64) -> Result<Cancellation, FirstSliceError> {
    if timeout_seconds == 0 {
        return Err(FirstSliceError::Limits);
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(timeout_seconds))
        .ok_or(FirstSliceError::Limits)?;
    Ok(Cancellation::with_deadline(deadline))
}

#[test]
#[ignore = "requires an explicit corpus, repository identity and fresh external diagnostic output"]
fn observe_repository_failed_generation_admission() {
    use super::{FirstSliceService, canonical_repository_root, repository_path_hash};
    use rootlight_ids::RepositoryId;

    let root = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_GENERATION_ADMISSION_ROOT")
            .expect("explicit corpus root is required"),
    );
    let output = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_GENERATION_ADMISSION_OUTPUT")
            .expect("explicit diagnostic output is required"),
    );
    let repository: RepositoryId = std::env::var("ROOTLIGHT_GENERATION_ADMISSION_REPOSITORY")
        .expect("explicit repository identity is required")
        .parse()
        .expect("repository identity is valid");
    let timeout_seconds = std::env::var("ROOTLIGHT_GENERATION_ADMISSION_TIMEOUT_SECONDS")
        .expect("explicit diagnostic timeout is required")
        .parse()
        .expect("diagnostic timeout is an integer number of seconds");
    let cancellation = census_cancellation(timeout_seconds).unwrap();
    let canonical = canonical_repository_root(&root, &cancellation).unwrap();
    let output = std::path::absolute(output).unwrap();
    let parent = std::fs::canonicalize(output.parent().expect("output has a parent")).unwrap();
    assert!(
        !parent.starts_with(&canonical),
        "output must be outside the corpus"
    );
    let output = parent.join(output.file_name().expect("output has a file name"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    service
        .restore_repository_registration(repository_path_hash(&canonical).unwrap(), repository)
        .unwrap();
    let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
    let result = service.prepare_repository_with_mode(
        &canonical,
        super::FirstSliceIndexMode::Structural,
        &cancellation,
    );
    if let Err(error) = &result {
        // Errors outside the narrow caller hooks remain explicitly unclassified.
        observe_failure(error);
    }
    let boundary_reached = guard.reached();
    let capture = guard.finish(&Cancellation::new());
    let observation = capture
        .as_ref()
        .ok()
        .map(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).unwrap());
    let report = serde_json::json!({
        "schema": "rootlight.failed-generation-admission-run/1",
        "actual_result": if result.is_err() { "failed" } else { "prepared" },
        "actual_error": result.as_ref().err().map(ErrorSummary::new),
        "boundary_reached": boundary_reached,
        "observation": observation,
        "observation_error": capture.as_ref().err().map(ErrorSummary::new),
        "service_mode": "ephemeral_structural_preparation",
        "requested_timeout_seconds": timeout_seconds,
        "durable_publication_qualified": false,
        "product_success_qualified": false,
        "benchmark_qualified": false,
        "whole_repository_observed": false,
        "source_text_included": false,
        "payload_contents_included": false,
    });
    let report = encode(&report, 2 * MAX_OBSERVATION_BYTES, &Cancellation::new()).unwrap();
    file.write_all(&report).unwrap();
    file.sync_all().unwrap();
    assert!(service.active_generation().is_none());
    assert!(service.active_generation_for(repository).is_none());
    assert!(
        matches!(
            result,
            Err(FirstSliceError::GenerationMemoryLimit { .. })
                | Err(FirstSliceError::ResourceLimit {
                    resource: FirstSliceResource::TextBytes,
                    ..
                })
        ),
        "the service returned a different boundary; inspect the saved diagnostic"
    );
    assert!(boundary_reached);
    assert!(capture.is_ok());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn census_cancellation_requires_a_representable_deadline() {
        let cancellation = census_cancellation(3600).unwrap();
        assert!(cancellation.has_deadline());
        super::super::check_cancellation(&cancellation).unwrap();
        cancellation.check().unwrap();
        assert!(matches!(
            census_cancellation(0),
            Err(FirstSliceError::Limits)
        ));
        assert!(matches!(
            census_cancellation(u64::MAX),
            Err(FirstSliceError::Limits)
        ));
    }

    #[test]
    fn temporary_reservation_hooks_do_not_charge_work_cumulatively() {
        let service = crate::FirstSliceService::new(2).unwrap();
        let generation = rootlight_ids::GenerationId::from_bytes([3; 20]);
        let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
        let mut reservation = service
            .reserve_generation_memory(generation, 100, crate::PendingGenerationMemory::Reserved)
            .unwrap();
        let mut admission = crate::generation_admission::GenerationAdmission::new(100).unwrap();
        let result = crate::compressed_chunk::with_optional_staging(
            &mut admission,
            &mut reservation,
            Ok(40),
            false,
            &Cancellation::new(),
            || Ok(7_u64),
            |bytes| Ok(*bytes),
        )
        .unwrap();
        assert_eq!(result, (Some(7), 140));
        assert_eq!(admission.reservation_bytes(), 107);
        let rejected = crate::compressed_chunk::with_optional_staging(
            &mut admission,
            &mut reservation,
            Ok(crate::MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES),
            false,
            &Cancellation::new(),
            || panic!("rejected temporary reservation must not execute work"),
            |_: &()| Ok(0),
        )
        .unwrap();
        assert_eq!(rejected, (None, 0));
        assert!(!guard.reached());
        assert_eq!(admission.reservation_bytes(), 107);
        observe_failure(&FirstSliceError::Limits);
        let value = report(guard);
        assert_eq!(value["costs"]["temporary_reservation_attempt_count"], 2);
        assert_eq!(value["costs"]["temporary_reservation_admitted_count"], 1);
        assert_eq!(
            value["costs"]["largest_temporary_reservation"]["admitted"],
            false
        );
        assert_eq!(value["costs"]["admitted_known_chunks"]["bytes"], 7);
        assert_eq!(value["costs"]["admitted_total_bytes"], 107);
    }

    #[test]
    fn borrowed_text_hooks_reconcile_only_the_matching_typed_boundary() {
        use rootlight_ids::{FactId, GenerationId, RepositoryId};
        use rootlight_ir::{
            ExtensionCriticality, ExtensionEnvelope, FactEvidence, NormalizedIrDocument,
        };

        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let mut document = NormalizedIrDocument::empty(repository, generation);
        document.extensions.push(ExtensionEnvelope {
            id: FactId::from_bytes([3; 20]),
            repository,
            generation,
            namespace: "test".to_owned(),
            version: "1".to_owned(),
            criticality: ExtensionCriticality::Noncritical,
            payload: "private-payload".to_owned(),
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: None,
                derivation: vec![],
            },
        });
        for observed in [4, 5] {
            let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
            let _phase = enter_phase(Phase::FinalVerification);
            observe_text_document(
                &document,
                &rootlight_storage::SourceFileCatalog::default(),
                3,
                &Cancellation::new(),
            );
            let original = FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::TextBytes,
                observed,
                limit: 3,
            };
            observe_failure(&original);
            let result = guard.finish(&Cancellation::new());
            if observed == 4 {
                let encoded = result.unwrap();
                let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
                assert_eq!(
                    value["costs"]["text_census"]["reconciled_terminal_error"]["observed"],
                    4
                );
                assert!(
                    !String::from_utf8(encoded)
                        .unwrap()
                        .contains("private-payload")
                );
            } else {
                assert_eq!(result, Err(FirstSliceError::Identity));
            }
            assert_eq!(
                original,
                FirstSliceError::ResourceLimit {
                    resource: FirstSliceResource::TextBytes,
                    observed,
                    limit: 3
                }
            );
        }
    }
    use rootlight_cancel::CancellationReason;

    fn report(guard: CaptureGuard) -> serde_json::Value {
        serde_json::from_slice(&guard.finish(&Cancellation::new()).unwrap()).unwrap()
    }

    #[test]
    fn disabled_hooks_and_successful_work_never_seal_a_capture() {
        observe_source_floor(100);
        observe_admitted(u64::MAX, 0);
        observe_failure(&FirstSliceError::Limits);
        let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
        begin_admission(100);
        observe_admitted(5, 105);
        assert!(!guard.reached());
        assert_eq!(
            guard.finish(&Cancellation::new()),
            Err(FirstSliceError::Identity)
        );
    }

    #[test]
    fn nested_captures_and_multiple_admissions_are_rejected() {
        let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
        assert!(matches!(
            CaptureGuard::arm(1),
            Err(FirstSliceError::Identity)
        ));
        begin_admission(100);
        begin_admission(100);
        observe_failure(&FirstSliceError::Limits);
        assert_eq!(
            guard.finish(&Cancellation::new()),
            Err(FirstSliceError::Identity)
        );
        assert!(matches!(
            CaptureGuard::arm(MAX_OBSERVATION_BYTES + 1),
            Err(FirstSliceError::Identity)
        ));
    }

    #[test]
    fn document_and_chunk_commits_conserve_the_floor_without_double_counting() {
        let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
        begin_admission(100);
        {
            let _charge = enter_document_charge();
            observe_admitted(20, 120);
        }
        {
            let _phase = enter_phase(Phase::OptionalChunk);
            observe_admitted(7, 127);
        }
        observe_known_attempt(11);
        assert!(!guard.reached());
        observe_failure(&FirstSliceError::Limits);
        let value = report(guard);
        assert_eq!(
            value["costs"]["admitted_documents"],
            serde_json::json!({"count":1,"bytes":20})
        );
        assert_eq!(
            value["costs"]["admitted_known_chunks"],
            serde_json::json!({"count":1,"bytes":7})
        );
        assert_eq!(
            value["costs"]["admitted_known_bytes"],
            serde_json::json!({"count":1,"bytes":7})
        );
        assert_eq!(value["costs"]["admitted_total_bytes"], 127);
        assert_eq!(value["failure_phase"], "unclassified");
        for key in [
            "source_text_included",
            "payload_contents_included",
            "owned_heap_measured",
            "process_rss_measured",
            "process_wide_census",
            "whole_repository_observed",
            "product_success_qualified",
            "benchmark_qualified",
        ] {
            assert_eq!(value[key], false);
        }
    }

    #[test]
    fn diagnostic_overflow_and_bad_conservation_do_not_partially_commit() {
        for overflow in [false, true] {
            let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
            begin_admission(if overflow { u64::MAX } else { 100 });
            observe_admitted(1, 102);
            CAPTURE.with(|slot| {
                let slot = slot.borrow();
                let state = slot.as_ref().unwrap();
                assert_eq!(state.costs.admitted_known_chunks, Usage::default());
                assert_eq!(state.costs.admitted_known_bytes, Usage::default());
                assert_eq!(
                    state.diagnostic_error,
                    Some(if overflow {
                        FirstSliceError::Limits
                    } else {
                        FirstSliceError::Identity
                    })
                );
            });
            drop(guard);
        }
        assert_eq!(
            Usage {
                count: u64::MAX,
                bytes: 0
            }
            .added(1),
            Err(FirstSliceError::Limits)
        );
        assert_eq!(
            Usage {
                count: 0,
                bytes: u64::MAX
            }
            .added(1),
            Err(FirstSliceError::Limits)
        );
    }

    #[test]
    fn peak_components_are_conserved_and_optional_rejection_is_not_terminal() {
        let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
        let mut peak = TemporaryPeak {
            base_bytes: 100,
            document_bytes: 2,
            workspace_bytes: 3,
            descriptor_bytes: 4,
            work_bytes: 15,
            peak_bytes: 115,
            admitted: true,
        };
        observe_temporary_peak(peak);
        peak.peak_bytes = 215;
        peak.base_bytes = 200;
        peak.admitted = false;
        observe_temporary_peak(peak);
        let projection = ProjectionPeak {
            document_bytes: 2,
            lexical_coverage_bytes: 1,
            workspace_bytes: 3,
            snapshot_metadata_bytes: 0,
            structural_cache_bytes: 4,
            explain_index_bytes: 5,
            peak_bytes: 21,
        };
        observe_projection_peak(projection);
        assert!(!guard.reached());
        observe_failure(&FirstSliceError::Limits);
        let value = report(guard);
        assert_eq!(value["costs"]["temporary_attempt_count"], 2);
        assert_eq!(value["costs"]["temporary_admitted_count"], 1);
        assert_eq!(
            value["costs"]["largest_temporary_attempt"]["peak_bytes"],
            215
        );
        assert_eq!(
            value["costs"]["largest_admitted_temporary_peak"]["peak_bytes"],
            115
        );
        assert_eq!(
            TemporaryPeak {
                work_bytes: 0,
                ..peak
            }
            .validate(),
            Err(FirstSliceError::Identity)
        );
        assert_eq!(
            TemporaryPeak {
                document_bytes: u64::MAX,
                ..peak
            }
            .validate(),
            Err(FirstSliceError::Limits)
        );
        assert_eq!(
            ProjectionPeak {
                peak_bytes: 0,
                ..projection
            }
            .validate(),
            Err(FirstSliceError::Identity)
        );
        assert_eq!(
            ProjectionPeak {
                document_bytes: u64::MAX,
                ..projection
            }
            .validate(),
            Err(FirstSliceError::Limits)
        );
    }

    #[test]
    fn phase_guards_restore_across_return_and_unwind_and_sealing_copies_phase() {
        fn early_return(stop: bool) {
            let phase = enter_phase(Phase::OptionalChunk);
            if stop {
                return;
            }
            drop(phase);
        }
        let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
        let outer = enter_phase(Phase::FileDocument);
        early_return(true);
        assert!(
            std::panic::catch_unwind(|| {
                let _phase = enter_phase(Phase::ProjectDocument);
                panic!("synthetic unwind");
            })
            .is_err()
        );
        CAPTURE.with(|slot| assert_eq!(slot.borrow().as_ref().unwrap().phase, Phase::FileDocument));
        observe_failure(&FirstSliceError::Limits);
        drop(outer);
        assert_eq!(report(guard)["failure_phase"], "file_document");
        let _disabled = enter_phase(Phase::SourcePreflight);
    }

    #[test]
    fn stale_phase_and_charge_guards_cannot_change_a_later_capture() {
        let first = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
        let phase = enter_phase(Phase::FinalProjection);
        let charge = enter_document_charge();
        observe_failure(&FirstSliceError::Limits);
        report(first);
        let second = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
        let _current = enter_phase(Phase::FinalVerification);
        drop(phase);
        drop(charge);
        begin_admission(0);
        observe_admitted(1, 1);
        observe_failure(&FirstSliceError::Limits);
        let value = report(second);
        assert_eq!(value["failure_phase"], "final_verification");
        assert_eq!(value["costs"]["admitted_known_bytes"]["bytes"], 1);
        assert_eq!(value["costs"]["admitted_known_chunks"]["bytes"], 0);
        assert_eq!(value["costs"]["admitted_documents"]["bytes"], 0);
    }

    #[test]
    fn reporting_bounds_and_cancellation_never_replace_the_original_error() {
        let original = FirstSliceError::Limits;
        for cancelled in [false, true] {
            let guard =
                CaptureGuard::arm(if cancelled { MAX_OBSERVATION_BYTES } else { 8 }).unwrap();
            observe_failure(&original);
            let cancellation = Cancellation::new();
            if cancelled {
                cancellation.cancel(CancellationReason::ClientRequest);
            }
            let result = guard.finish(&cancellation);
            if cancelled {
                assert_eq!(
                    result,
                    Err(FirstSliceError::Cancelled(
                        CancellationReason::ClientRequest
                    ))
                );
            } else {
                assert!(matches!(result, Err(FirstSliceError::ResourceLimit {
                    resource: FirstSliceResource::OutputBytes, observed, limit: 8,
                }) if observed > 8));
            }
            assert_eq!(original, FirstSliceError::Limits);
        }
    }
}

#[cfg(test)]
mod projection_metadata_tests {
    use super::*;

    #[test]
    fn snapshot_metadata_is_conserved_in_projection_peak_and_overflow_is_strict() {
        let peak = ProjectionPeak {
            document_bytes: 2,
            lexical_coverage_bytes: 1,
            workspace_bytes: 3,
            snapshot_metadata_bytes: 7,
            structural_cache_bytes: 4,
            explain_index_bytes: 5,
            peak_bytes: 28,
        };
        assert_eq!(peak.validate(), Ok(()));
        assert_eq!(
            ProjectionPeak {
                peak_bytes: 21,
                ..peak
            }
            .validate(),
            Err(FirstSliceError::Identity)
        );
        assert_eq!(
            ProjectionPeak {
                snapshot_metadata_bytes: u64::MAX,
                ..peak
            }
            .validate(),
            Err(FirstSliceError::Limits)
        );
    }
}
