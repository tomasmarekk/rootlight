//! Explicit generation-owned packed read reuse with independently charged owners.
//! Ordinary source reads remain independently fresh. Responses are borrowed only
//! during the caller's provisional callback, outside service and pack locks.

use crate::*;
use rootlight_source::{SourceService, VerifiedSourceBacking};
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
}

impl OperationWork {
    #[cfg(test)]
    pub(crate) fn for_test(maximum: u64) -> Self {
        Self {
            remaining: Cell::new(maximum),
        }
    }
    pub(crate) fn charge(
        &self,
        amount: u64,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        check_cancellation(cancellation)?;
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
            self.authorize()?;
            let response = self.execute(references, options, budget)?;
            self.authorize()?;
            visitor(&response)?;
            self.authorize()?;
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
        ensure_memory(request_bytes, self.limits)?;
        owner.reservation.resize(request_bytes, true)?;
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
        let query = owner
            .lease
            .query()
            .map_err(|error| map_query_error(error, self.cancellation))?;
        if streamed {
            let backing = OperationBacking {
                service: self.service,
                generation,
                reader: &owner.reader,
                work: &self.work,
            };
            let source = SourceService::from_verified_backing(&backing, generation)
                .map_err(|error| map_source_error(error, self.cancellation))?;
            let plan = query
                .plan_source_read(references, options, budget.source(), budget.query())
                .map_err(|error| map_query_error(error, self.cancellation))?;
            return query
                .execute_source_read(&plan, &source, self.cancellation)
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
        let plan = query
            .plan_source_read(references, options, budget.source(), budget.query())
            .map_err(|error| map_query_error(error, self.cancellation))?;
        query
            .execute_source_read(&plan, &source, self.cancellation)
            .map_err(|error| map_query_error(error, self.cancellation))
    }
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
