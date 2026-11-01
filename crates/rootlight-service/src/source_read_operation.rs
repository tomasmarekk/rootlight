//! Explicit generation-owned packed read reuse with independently charged owners.
//! Ordinary source reads remain independently fresh. Responses are borrowed only
//! during the caller's provisional callback, outside service and pack locks.

use crate::*;
use rootlight_source::{PreparedUtf8Source, SourceService, VerifiedSourceBacking};
use std::{cell::Cell, fmt};

/// Lifetime bounds for an explicitly opened packed source read operation.
///
/// Work charges byte/entry upper bounds for storage verification plus each
/// request's query row/memory/JSON ceilings. Charges are cumulative across
/// requests, including failed requests and cache hits; they are not timing data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceSourceReadOperationLimits {
    maximum_reads: u64,
    maximum_work: u64,
    maximum_memory_bytes: u64,
}

impl FirstSliceSourceReadOperationLimits {
    /// Sets finite operation bounds without widening any per-request budget.
    ///
    /// # Errors
    /// Returns [`FirstSliceError::Limits`] for zero or an unsupported memory bound.
    pub fn new(
        maximum_reads: u64,
        maximum_work: u64,
        maximum_memory_bytes: u64,
    ) -> Result<Self, FirstSliceError> {
        if maximum_reads == 0
            || maximum_work == 0
            || maximum_memory_bytes == 0
            || maximum_memory_bytes > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES
        {
            return Err(FirstSliceError::Limits);
        }
        Ok(Self {
            maximum_reads,
            maximum_work,
            maximum_memory_bytes,
        })
    }
}

pub(crate) struct OperationWork {
    remaining: Cell<u64>,
    deadline: Cell<Option<std::time::Instant>>,
}

impl OperationWork {
    #[cfg(test)]
    pub(crate) fn for_test(maximum: u64) -> Self {
        Self {
            remaining: Cell::new(maximum),
            deadline: Cell::new(None),
        }
    }
    pub(crate) fn charge(
        &self,
        amount: u64,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        check_cancellation(cancellation)?;
        if self
            .deadline
            .get()
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Err(FirstSliceError::Cancelled(
                CancellationReason::DeadlineExceeded,
            ));
        }
        let remaining = self
            .remaining
            .get()
            .checked_sub(amount)
            .ok_or(FirstSliceError::BudgetExceeded)?;
        self.remaining.set(remaining);
        Ok(())
    }
}

struct Owner {
    lease: source_backing::ServiceGenerationLease,
    reader: durable::read_operation::PackedReadOperation,
    reservation: RestoredMemoryReservation,
    held_bytes: u64,
    prepared: Option<PreparedRead>,
}

struct PreparedRead {
    view: PreparedUtf8Source,
    _reservation: RestoredMemoryReservation,
    bytes: u64,
}

/// One explicitly authorized immutable view of a loaded packed generation.
///
/// The operation never pins logical or durable publication indefinitely. Each
/// read rechecks both authorities before and after staged execution and after
/// its provisional callback. External retirement may therefore refuse a read;
/// holding this owner does not promise atomic retention across processes.
pub struct FirstSliceSourceReadOperation<'service> {
    service: &'service FirstSliceService,
    cancellation: &'service Cancellation,
    receipt: FirstSliceIndexReceipt,
    limits: FirstSliceSourceReadOperationLimits,
    work: OperationWork,
    reads: u64,
    owner: Option<Owner>,
    failure: Option<FirstSliceError>,
}

impl fmt::Debug for FirstSliceSourceReadOperation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FirstSliceSourceReadOperation")
            .field("generation", &self.receipt.generation)
            .field("reads", &self.reads)
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}

impl FirstSliceService {
    /// Opens bounded index/one-pack reuse for an already loaded durable packed generation.
    ///
    /// `None` means this backing is not applicable; the caller can separately
    /// choose an ordinary source read outside this operation's reuse contract.
    /// No unloaded generation is restored to obtain applicability. Caller
    /// cancellation and any optional deadline remain authoritative throughout.
    /// The supplied memory limit is charged independently of the generation.
    ///
    /// # Errors
    /// Returns existing typed cancellation, retention, publication, integrity or
    /// resource failures. No operation is returned after partial initialization.
    pub fn source_read_operation<'service>(
        &'service self,
        generation: GenerationId,
        limits: FirstSliceSourceReadOperationLimits,
        cancellation: &'service Cancellation,
    ) -> Result<Option<FirstSliceSourceReadOperation<'service>>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let Some(durable) = &self.durable else {
            return Ok(None);
        };
        let lease = {
            let mut cache = self
                .generation_cache
                .lock()
                .map_err(|_| FirstSliceError::Retention)?;
            if !cache.generations.contains(generation) {
                return Err(FirstSliceError::Query);
            }
            if !cache.generations.is_loaded(generation) {
                return Ok(None);
            }
            // Retain the loaded payload before another reservation can unload it.
            source_backing::ServiceGenerationLease::new(
                cache.lease(generation)?,
                self.streamed_sources.get(&generation).cloned(),
            )
        };
        let receipt = self
            .receipts
            .get(&generation)
            .cloned()
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let work = OperationWork {
            remaining: Cell::new(limits.maximum_work),
            deadline: Cell::new(None),
        };
        // This precedes directory/manifest decoding and bounds that workspace.
        let base = GENERATION_MEMORY_FIXED_OVERHEAD_BYTES;
        ensure_memory(base, limits)?;
        let mut reservation =
            RestoredMemoryReservation::new(Arc::clone(&self.generation_cache), base, true)?;
        let Some(descriptor) = durable.read_operation_descriptor(&receipt, &work, cancellation)?
        else {
            return Ok(None);
        };
        let held_bytes = base
            .checked_add(descriptor.memory_bytes()?)
            .ok_or(FirstSliceError::Limits)?;
        ensure_memory(held_bytes, limits)?;
        reservation.resize(held_bytes, true)?;
        let reader = durable.open_read_operation(&receipt, descriptor, &work, cancellation)?;
        let operation = FirstSliceSourceReadOperation {
            service: self,
            cancellation,
            receipt,
            limits,
            work,
            reads: 0,
            owner: Some(Owner {
                lease,
                reader,
                reservation,
                held_bytes,
                prepared: None,
            }),
            failure: None,
        };
        operation.authorize()?;
        Ok(Some(operation))
    }
}

impl FirstSliceSourceReadOperation<'_> {
    #[cfg(test)]
    pub(crate) fn test_io_counts(&self) -> (u64, u64, u64) {
        self.owner
            .as_ref()
            .expect("test operation remains open")
            .reader
            .counters()
    }
    /// Runs the unchanged query/source pipeline against this operation's verified view.
    ///
    /// `visitor` borrows a staged response without any service/cache/pack lock.
    /// Its effects must remain provisional until this method returns `Ok(())`;
    /// a callback error or final publication failure cannot expose success.
    /// Caller-owned copies made by the callback are outside this reservation.
    /// A first failure closes the owner immediately and is returned on later reads.
    ///
    /// # Errors
    /// Returns the original typed execution/callback failure, current publication
    /// refusal, cancellation or an operation/per-request resource refusal.
    pub fn read(
        &mut self,
        references: Vec<SourceRef>,
        options: SourceReadOptions,
        budget: FirstSliceBudget,
        visitor: impl FnOnce(&QueryResponse<SourceReadQueryResult>) -> Result<(), FirstSliceError>,
    ) -> Result<(), FirstSliceError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = (|| {
            check_cancellation(self.cancellation)?;
            self.reads = self
                .reads
                .checked_add(1)
                .filter(|count| *count <= self.limits.maximum_reads)
                .ok_or(FirstSliceError::BudgetExceeded)?;
            let owner = self.owner.as_ref().ok_or(FirstSliceError::Retention)?;
            let source_budget = budget.source();
            let query_budget = budget.query();
            let duration = if streamed_request(self.service, owner.lease.generation(), &references)
            {
                match (
                    query_budget.explicit_max_duration(),
                    source_budget.max_verification_duration,
                ) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                }
            } else {
                Some(query_budget.max_duration().min(source_budget.max_duration))
            };
            let started = std::time::Instant::now();
            self.work
                .deadline
                .set(duration.map(|duration| started.checked_add(duration).unwrap_or(started)));
            profile_analysis_phase!(SourceReadInitialAuthority, None, self.authorize())?;
            let response = self.execute(references, options, budget)?;
            profile_analysis_phase!(SourceReadFinalAuthority, None, self.authorize())?;
            profile_analysis_phase!(SourceReadCallback, None, visitor(&response))?;
            profile_analysis_phase!(SourceReadCallbackAuthority, None, self.authorize())?;
            drop(response);
            let owner = self.owner.as_mut().ok_or(FirstSliceError::Retention)?;
            owner.reservation.resize(owner.held_bytes, true)?;
            Ok(())
        })();
        if let Err(error) = result {
            self.owner = None;
            self.failure = Some(error);
        }
        result
    }

    fn authorize(&self) -> Result<(), FirstSliceError> {
        check_cancellation(self.cancellation)?;
        let owner = self.owner.as_ref().ok_or(FirstSliceError::Retention)?;
        let identity = owner.lease.generation().metadata();
        {
            let cache = self
                .service
                .generation_cache
                .lock()
                .map_err(|_| FirstSliceError::Retention)?;
            if !cache.generations.contains(self.receipt.generation) {
                return Err(FirstSliceError::Query);
            }
            if cache
                .generations
                .metadata(self.receipt.generation)
                .map_err(|_| FirstSliceError::Query)?
                != identity
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }
        if self.service.receipts.get(&self.receipt.generation) != Some(&self.receipt) {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        owner.reader.revalidate(
            self.service
                .durable
                .as_ref()
                .ok_or(FirstSliceError::CatalogCorrupt)?,
            &self.receipt,
            &self.work,
            self.cancellation,
        )
    }

    fn execute(
        &mut self,
        references: Vec<SourceRef>,
        options: SourceReadOptions,
        budget: FirstSliceBudget,
    ) -> Result<QueryResponse<SourceReadQueryResult>, FirstSliceError> {
        if references.len() > rootlight_source::HARD_MAX_SOURCE_SELECTORS {
            return Err(FirstSliceError::BudgetExceeded);
        }
        let owner = self.owner.as_mut().ok_or(FirstSliceError::Retention)?;
        let generation = owner.lease.generation();
        let mut selected_bytes = 0u64;
        for (index, reference) in references.iter().enumerate() {
            check_cancellation(self.cancellation)?;
            if !references
                .get(..index)
                .ok_or(FirstSliceError::Limits)?
                .iter()
                .any(|earlier| earlier.span().file() == reference.span().file())
                && let Some(file) = generation.find_file(reference.span().file())
            {
                selected_bytes = selected_bytes
                    .checked_add(file.byte_length)
                    .ok_or(FirstSliceError::Limits)?;
            }
        }
        let request_work = selected_bytes
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(budget.query().max_rows()))
            .and_then(|bytes| bytes.checked_add(budget.query().max_memory_bytes()))
            .and_then(|bytes| bytes.checked_add(budget.query().max_json_bytes()))
            .ok_or(FirstSliceError::Limits)?;
        self.work.charge(request_work, self.cancellation)?;
        let request_bytes = owner
            .held_bytes
            .checked_add(selected_bytes)
            .and_then(|bytes| {
                budget
                    .query()
                    .max_memory_bytes()
                    .checked_mul(2)
                    .and_then(|response| bytes.checked_add(response))
            })
            .ok_or(FirstSliceError::Limits)?;
        let view_file = references.first().and_then(|reference| {
            (references.len() == 1
                && options.encoding == rootlight_source::SourceEncoding::Utf8
                && options.context_lines_before == 0
                && options.context_lines_after == 0)
                .then(|| generation.find_file(reference.span().file()))
                .flatten()
                .filter(|file| {
                    matches!(file.encoding.as_str(), "utf-8" | "utf8")
                        && file.byte_length <= rootlight_vfs::MAX_SNAPSHOT_BYTES
                        && (streamed_request(self.service, generation, &references)
                            || file.byte_length <= budget.source().max_snapshot_bytes)
                })
        });
        if owner.prepared.as_ref().is_some_and(|prepared| {
            view_file.is_none_or(|file| prepared.view.snapshot().file() != file.id)
                || request_bytes
                    .checked_add(prepared.bytes)
                    .is_none_or(|bytes| bytes > self.limits.maximum_memory_bytes)
        }) {
            owner.prepared = None;
        }
        ensure_memory(request_bytes, self.limits)?;
        if let Err(error) = owner.reservation.resize(request_bytes, true) {
            if matches!(error, FirstSliceError::GenerationMemoryLimit { .. })
                && owner.prepared.take().is_some()
            {
                owner.reservation.resize(request_bytes, true)?;
            } else {
                return Err(error);
            }
        }
        if owner.prepared.is_none()
            && let Some(file) = view_file
        {
            let snapshot = operation_snapshot(
                self.service,
                &owner.reader,
                generation,
                file,
                &self.work,
                self.cancellation,
            )?;
            let upper = PreparedUtf8Source::memory_upper(&snapshot)
                .and_then(|bytes| u64::try_from(bytes).ok())
                .ok_or(FirstSliceError::Limits)?;
            // The request already owns snapshot construction. This separate
            // charge covers the view before checkpoint allocation and remains
            // live after the request workspace is released.
            if request_bytes
                .checked_add(upper)
                .is_some_and(|bytes| bytes <= self.limits.maximum_memory_bytes)
            {
                let admitted = match RestoredMemoryReservation::new(
                    Arc::clone(&self.service.generation_cache),
                    upper,
                    true,
                ) {
                    Ok(reservation) => Some(reservation),
                    Err(FirstSliceError::GenerationMemoryLimit { .. }) => None,
                    Err(error) => return Err(error),
                };
                if let Some(mut reservation) = admitted {
                    match profile_analysis_phase!(
                        SourceReadPreparedUtf8,
                        Some(file.id),
                        PreparedUtf8Source::new(
                            snapshot,
                            generation,
                            self.cancellation,
                            self.work.deadline.get(),
                        )
                    ) {
                        Ok(view) => {
                            let bytes = view
                                .retained_memory_bytes()
                                .and_then(|bytes| u64::try_from(bytes).ok())
                                .filter(|bytes| *bytes <= upper)
                                .ok_or(FirstSliceError::Limits)?;
                            reservation.resize(bytes, true)?;
                            owner.prepared = Some(PreparedRead {
                                view,
                                _reservation: reservation,
                                bytes,
                            });
                        }
                        Err(rootlight_source::SourceError::MemoryUnavailable) => {}
                        Err(error) => return Err(map_source_error(error, self.cancellation)),
                    }
                }
            }
        }
        let snapshot_limit = self
            .service
            .config
            .analysis()
            .max_source_file_bytes
            .min(rootlight_vfs::MAX_SNAPSHOT_BYTES);
        let streamed = references.iter().any(|reference| {
            generation
                .find_file(reference.span().file())
                .is_some_and(|file| {
                    file.byte_length > snapshot_limit
                        || file.encoding == "bytes"
                        || self
                            .service
                            .streamed_sources
                            .get(&self.receipt.generation)
                            .is_some_and(|sources| sources.contains(file.id))
                })
        });
        let query = profile_analysis_phase!(SourceReadQueryOpen, None, owner.lease.query())
            .map_err(|error| map_query_error(error, self.cancellation))?;
        if let Some(prepared) = &owner.prepared {
            let source = SourceService::from_prepared_utf8(&prepared.view, generation)
                .map_err(|error| map_source_error(error, self.cancellation))?;
            let plan = profile_analysis_phase!(
                SourceReadQueryPlan,
                None,
                query.plan_source_read(references, options, budget.source(), budget.query())
            )
            .map_err(|error| map_query_error(error, self.cancellation))?;
            return profile_analysis_phase!(
                SourceReadQueryExecute,
                None,
                query.execute_source_read(&plan, &source, self.cancellation)
            )
            .map_err(|error| map_query_error(error, self.cancellation));
        }
        if streamed {
            let backing = OperationBacking {
                service: self.service,
                generation,
                reader: &owner.reader,
                work: &self.work,
            };
            let source = SourceService::from_verified_backing(&backing, generation)
                .map_err(|error| map_source_error(error, self.cancellation))?;
            let plan = profile_analysis_phase!(
                SourceReadQueryPlan,
                None,
                query.plan_source_read(references, options, budget.source(), budget.query())
            )
            .map_err(|error| map_query_error(error, self.cancellation))?;
            return profile_analysis_phase!(
                SourceReadQueryExecute,
                None,
                query.execute_source_read(&plan, &source, self.cancellation)
            )
            .map_err(|error| map_query_error(error, self.cancellation));
        }
        let retained = self
            .service
            .source_snapshots
            .snapshots(self.receipt.generation)
            .ok_or(FirstSliceError::Query)?;
        let mut selected = Vec::new();
        selected
            .try_reserve_exact(references.len())
            .map_err(|_| FirstSliceError::Retention)?;
        let mut previous = None;
        loop {
            check_cancellation(self.cancellation)?;
            let next = references
                .iter()
                .map(|reference| reference.span().file())
                .filter(|file| previous.is_none_or(|previous| *file > previous))
                .min();
            let Some(file) = next else {
                break;
            };
            selected.push(
                match retained.binary_search_by_key(&file, |source| source.file()) {
                    Ok(index) => Arc::clone(retained.get(index).ok_or(FirstSliceError::Query)?),
                    Err(_) => Arc::new(owner.reader.read(
                        generation.find_file(file).ok_or(FirstSliceError::Query)?,
                        &self.work,
                        self.cancellation,
                    )?),
                },
            );
            previous = Some(file);
        }
        let source = SourceService::from_snapshots(&selected, generation)
            .map_err(|error| map_source_error(error, self.cancellation))?;
        let plan = profile_analysis_phase!(
            SourceReadQueryPlan,
            None,
            query.plan_source_read(references, options, budget.source(), budget.query())
        )
        .map_err(|error| map_query_error(error, self.cancellation))?;
        profile_analysis_phase!(
            SourceReadQueryExecute,
            None,
            query.execute_source_read(&plan, &source, self.cancellation)
        )
        .map_err(|error| map_query_error(error, self.cancellation))
    }
}

fn operation_snapshot(
    service: &FirstSliceService,
    reader: &durable::read_operation::PackedReadOperation,
    generation: &rootlight_storage::GenerationSnapshot,
    file: &FileRecord,
    work: &OperationWork,
    cancellation: &Cancellation,
) -> Result<Arc<rootlight_vfs::SourceSnapshot>, FirstSliceError> {
    if service
        .streamed_sources
        .get(&file.generation)
        .is_some_and(|sources| sources.contains(file.id))
    {
        let profile = AnalysisProfileGuard::begin(
            AnalysisProfilePhase::SourceReadStreamedSnapshot,
            Some(file.id),
        );
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(
                usize::try_from(file.byte_length).map_err(|_| FirstSliceError::Limits)?,
            )
            .map_err(|_| FirstSliceError::Retention)?;
        let backing = crate::source_backing::GenerationBacking::new(service, generation);
        backing
            .visit_verified(
                file,
                file.byte_length,
                cancellation,
                &mut |offset, chunk| {
                    work.charge(0, cancellation).map_err(|error| match error {
                        FirstSliceError::Cancelled(reason) => {
                            rootlight_source::SourceError::Cancelled(reason)
                        }
                        _ => rootlight_source::SourceError::VerificationBudgetExceeded,
                    })?;
                    if usize::try_from(offset).ok() != Some(bytes.len()) {
                        return Err(rootlight_source::SourceError::StaleSource);
                    }
                    if bytes.len().checked_add(chunk.len()).is_none_or(|end| {
                        u64::try_from(end)
                            .ok()
                            .is_none_or(|end| end > file.byte_length)
                    }) {
                        return Err(rootlight_source::SourceError::StaleSource);
                    }
                    bytes.extend_from_slice(chunk);
                    Ok(())
                },
            )
            .map_err(|error| map_source_error(error, cancellation))?;
        let path = rootlight_vfs::RelativePath::from_locator(
            file.path_locator
                .as_ref()
                .ok_or(FirstSliceError::CatalogCorrupt)?,
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let result = rootlight_vfs::SourceSnapshot::from_persisted(
            file.repository,
            path,
            file.id,
            file.content_hash,
            bytes,
        )
        .map(Arc::new)
        .map_err(|_| FirstSliceError::CatalogCorrupt);
        profile.finish();
        return result;
    }
    if let Some(sources) = service.source_snapshots.snapshots(file.generation)
        && let Ok(index) = sources.binary_search_by_key(&file.id, |source| source.file())
    {
        return profile_analysis_phase!(
            SourceReadRetainedSnapshot,
            Some(file.id),
            sources
                .get(index)
                .map(Arc::clone)
                .ok_or(FirstSliceError::Query)
        );
    }
    profile_analysis_phase!(
        SourceReadPackedSnapshot,
        Some(file.id),
        reader.read(file, work, cancellation)
    )
    .map(Arc::new)
}

fn streamed_request(
    service: &FirstSliceService,
    generation: &rootlight_storage::GenerationSnapshot,
    references: &[SourceRef],
) -> bool {
    let maximum = service
        .config
        .analysis()
        .max_source_file_bytes
        .min(rootlight_vfs::MAX_SNAPSHOT_BYTES);
    references.iter().any(|reference| {
        generation
            .find_file(reference.span().file())
            .is_some_and(|file| {
                file.byte_length > maximum
                    || file.encoding == "bytes"
                    || service
                        .streamed_sources
                        .get(&file.generation)
                        .is_some_and(|sources| sources.contains(file.id))
            })
    })
}

fn ensure_memory(
    bytes: u64,
    limits: FirstSliceSourceReadOperationLimits,
) -> Result<(), FirstSliceError> {
    if bytes > limits.maximum_memory_bytes {
        return Err(FirstSliceError::ResourceLimit {
            resource: FirstSliceResource::MemoryBytes,
            observed: bytes,
            limit: limits.maximum_memory_bytes,
        });
    }
    Ok(())
}

struct OperationBacking<'a> {
    service: &'a FirstSliceService,
    generation: &'a GenerationSnapshot,
    reader: &'a durable::read_operation::PackedReadOperation,
    work: &'a OperationWork,
}

impl fmt::Debug for OperationBacking<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperationBacking")
            .finish_non_exhaustive()
    }
}

impl VerifiedSourceBacking for OperationBacking<'_> {
    fn repository(&self) -> RepositoryId {
        self.generation.metadata().repository()
    }
    fn generation(&self) -> GenerationId {
        self.generation.metadata().generation()
    }
    fn visit_verified(
        &self,
        file: &FileRecord,
        maximum: u64,
        cancellation: &Cancellation,
        visitor: &mut dyn FnMut(u64, &[u8]) -> Result<(), SourceError>,
    ) -> Result<u64, SourceError> {
        if file.repository != self.repository()
            || file.generation != self.generation()
            || self.generation.find_file(file.id) != Some(file)
        {
            return Err(SourceError::StaleSource);
        }
        if file.byte_length > maximum {
            return Err(SourceError::VerificationBudgetExceeded);
        }
        if self
            .service
            .streamed_sources
            .get(&self.generation())
            .is_some_and(|owner| owner.contains(file.id))
            || self
                .service
                .source_snapshots
                .snapshots(self.generation())
                .is_some_and(|sources| {
                    sources
                        .binary_search_by_key(&file.id, |source| source.file())
                        .is_ok()
                })
        {
            // Existing captured/resident ownership and callback error precedence
            // remain authoritative; only durable misses use the operation index.
            let backing =
                crate::source_backing::GenerationBacking::new(self.service, self.generation);
            return backing.visit_verified(file, maximum, cancellation, visitor);
        }
        let mut first = None;
        let result = self.reader.visit(
            file,
            maximum,
            self.work,
            cancellation,
            &mut |offset, bytes| {
                if first.is_some() {
                    return Err(rootlight_vfs::platform::PlatformError::SecurityPolicy);
                }
                visitor(offset, bytes).map_err(|error| {
                    first = Some(error);
                    rootlight_vfs::platform::PlatformError::SecurityPolicy
                })
            },
        );
        if let Some(error) = first {
            return Err(error);
        }
        cancellation
            .check()
            .map_err(|error| SourceError::Cancelled(error.reason()))?;
        result.map_err(|error| match error {
            FirstSliceError::Cancelled(reason) => SourceError::Cancelled(reason),
            FirstSliceError::BudgetExceeded => SourceError::VerificationBudgetExceeded,
            _ => SourceError::StaleSource,
        })
    }
}

#[cfg(test)]
#[path = "tests/source_read_operation.rs"]
mod tests;
