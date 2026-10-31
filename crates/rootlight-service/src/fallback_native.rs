//! Retain bounded native source batches beside the durable exact-source catalog.
//! Unselected or capacity-refused files keep their original source-only records;
//! this bounded prefix does not establish repository-wide resolution completeness.

use super::*;

/// Native work completed for bounded attempts and their unpromoted source entries.
/// Only accepted fragments leave the entries; the remainder retains unknown analysis.
pub(super) struct NativeBatch {
    pub(super) catalog: Vec<SourceFileCatalogEntry>,
    pub(super) parsed_files: u64,
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct SourceBatchTrace {
    pub(super) events: Vec<SourceBatchEvent>,
    pub(super) captures: usize,
    pub(super) cancel_on_capture: Option<usize>,
}

#[cfg(test)]
pub(super) struct SourceBatchEvent {
    pub(super) phase: &'static str,
    pub(super) file: Option<FileId>,
    pub(super) ledger: u64,
    pub(super) source_allowance: u64,
    pub(super) source_batch_bytes: u64,
    pub(super) actual_reserved: u64,
    pub(super) highwater: u64,
}

#[cfg(test)]
#[expect(
    clippy::too_many_arguments,
    reason = "armed tests compare the actual reservation with independent source and retained allowances"
)]
fn observe(
    service: &FirstSliceService,
    memory: &GenerationMemoryReservation,
    phase: &'static str,
    file: Option<FileId>,
    ledger: u64,
    source_allowance: u64,
    source_batch_bytes: u64,
    cancellation: &Cancellation,
) -> Result<(), FirstSliceError> {
    let mut observed = service
        .native_source_batch_trace
        .lock()
        .map_err(|_| FirstSliceError::Retention)?;
    let Some(trace) = observed.as_mut() else {
        return Ok(());
    };
    if trace.events.len() >= 512 {
        return Err(FirstSliceError::Limits);
    }
    let actual_reserved = *memory
        .cache
        .lock()
        .map_err(|_| FirstSliceError::Retention)?
        .reservations
        .get(&memory.generation)
        .ok_or(FirstSliceError::Retention)?;
    let highwater = trace
        .events
        .last()
        .map_or(0, |event| event.highwater)
        .max(actual_reserved);
    trace.events.push(SourceBatchEvent {
        phase,
        file,
        ledger,
        source_allowance,
        source_batch_bytes,
        actual_reserved,
        highwater,
    });
    if phase == "capture" {
        trace.captures += 1;
        if trace.cancel_on_capture == Some(trace.captures) {
            cancellation.cancel(CancellationReason::ClientRequest);
        }
    }
    Ok(())
}

impl FirstSliceService {
    /// Promotes only verified native fragments whose complete append fits the current limits.
    /// Capacity refusals retain the exact catalog entry; other failures abort preparation.
    #[expect(
        clippy::too_many_arguments,
        reason = "the batch shares the existing source, output and memory admission owners"
    )]
    pub(super) fn append_fallback_native_batches(
        &self,
        root: &RepositoryRoot,
        inputs: &[ManifestInput],
        document: &mut NormalizedIrDocument,
        mut catalog: Vec<SourceFileCatalogEntry>,
        memory: &mut GenerationMemoryReservation,
        reserved_bytes: &mut u64,
        cancellation: &Cancellation,
    ) -> Result<NativeBatch, FirstSliceError> {
        #[cfg(not(test))]
        let (batch_files, batch_bytes) = (MAX_SNAPSHOT_BATCH_FILES, MAX_SNAPSHOT_BATCH_BYTES);
        #[cfg(test)]
        let (batch_files, batch_bytes) = self
            .native_source_batch_limits
            .unwrap_or((MAX_SNAPSHOT_BATCH_FILES, MAX_SNAPSHOT_BATCH_BYTES));
        if batch_files == 0
            || batch_files > MAX_SNAPSHOT_BATCH_FILES
            || batch_bytes == 0
            || batch_bytes > MAX_SNAPSHOT_BATCH_BYTES
        {
            return Err(FirstSliceError::Limits);
        }
        let maximum_bytes = batch_bytes.min(
            u64::try_from(self.source_snapshots.maximum_bytes)
                .map_err(|_| FirstSliceError::Limits)?,
        );
        let maximum_file_bytes = DiscoveryLimits::from_config(&self.config).max_file_bytes;
        let mut parsed_files = 0_u64;
        if maximum_bytes == 0 || self.analysis_limits.ir().max_files <= document.files.len() {
            return Ok(NativeBatch {
                catalog,
                parsed_files,
            });
        }
        let mut admission = generation_admission::GenerationAdmission::new(*reserved_bytes)?;
        admission.admit_document(document, memory, true, cancellation)?;
        let mut append = DocumentAppendState::from_document(document)?;
        append.aggregate_recovery = true;
        let mut promoted = BTreeSet::new();
        let mut cursor = 0;
        let mut source_allowance = 0_u64;
        while cursor < inputs.len() {
            let maximum_files = self
                .analysis_limits
                .ir()
                .max_files
                .saturating_sub(document.files.len())
                .min(batch_files);
            if maximum_files == 0 {
                break;
            }
            let mut selected = Vec::new();
            let mut requests = Vec::new();
            selected
                .try_reserve_exact(maximum_files)
                .map_err(|_| FirstSliceError::Limits)?;
            requests
                .try_reserve_exact(maximum_files)
                .map_err(|_| FirstSliceError::Limits)?;
            let mut source_bytes = 0_u64;
            while let Some(input) = inputs.get(cursor) {
                check_cancellation(cancellation)?;
                if selected.len() == maximum_files {
                    break;
                }
                if find(&catalog, input.file).is_none()
                    || supported_source_language(input, &self.analyzers).is_none()
                    || input.bytes > maximum_file_bytes
                    || usize::try_from(input.bytes).map_or(true, |bytes| {
                        bytes > self.analysis_limits.max_source_bytes()
                    })
                {
                    cursor += 1;
                    continue;
                }
                let footprint = input.bytes.max(1);
                if footprint > maximum_bytes {
                    cursor += 1;
                    continue;
                }
                let next = source_bytes
                    .checked_add(footprint)
                    .ok_or(FirstSliceError::Limits)?;
                if next > maximum_bytes {
                    break;
                }
                let file = find(&catalog, input.file).ok_or(FirstSliceError::Identity)?;
                let path = RelativePath::from_locator(
                    file.path_locator
                        .as_ref()
                        .ok_or(FirstSliceError::Identity)?,
                )
                .map_err(|error| map_vfs_error(error, cancellation))?;
                requests.push((path, input.bytes.max(1)));
                selected.push(input);
                source_bytes = next;
                cursor += 1;
            }
            if selected.is_empty() {
                break;
            }
            let required_allowance = source_bytes
                .checked_mul(GENERATION_MEMORY_SOURCE_PREFLIGHT_FACTOR)
                .ok_or(FirstSliceError::Limits)?;
            if required_allowance > source_allowance {
                if let Err(error) = admission.admit_known_bytes(
                    required_allowance - source_allowance,
                    memory,
                    true,
                    cancellation,
                ) {
                    #[cfg(test)]
                    observe(
                        self,
                        memory,
                        "workspace-refused",
                        None,
                        admission.reservation_bytes(),
                        source_allowance,
                        0,
                        cancellation,
                    )?;
                    if capacity_refusal(&error) {
                        break;
                    }
                    return Err(error);
                }
                source_allowance = required_allowance;
            }
            if let Err(error) = memory.resize(
                admission.reservation_bytes(),
                PendingGenerationMemory::Staged,
                true,
            ) {
                if capacity_refusal(&error) {
                    break;
                }
                return Err(error);
            }
            *reserved_bytes = (*reserved_bytes).max(admission.reservation_bytes());
            // The single ledger keeps every output charge and only the largest
            // source workspace. The owning scope must close before releasing it.
            let outcome = (|| -> Result<bool, FirstSliceError> {
                let snapshots = root
                    .snapshot_batch_with_cancellation(&requests, cancellation)
                    .map_err(|error| map_vfs_error(error, cancellation))?;
                if snapshots.len() != selected.len() {
                    return Err(FirstSliceError::DiscoveryDrift);
                }
                drop(requests);
                #[cfg(test)]
                observe(
                    self,
                    memory,
                    "capture",
                    None,
                    admission.reservation_bytes(),
                    source_allowance,
                    source_bytes,
                    cancellation,
                )?;
                for (input, snapshot) in selected.into_iter().zip(snapshots) {
                    check_cancellation(cancellation)?;
                    let snapshot = snapshot.map_err(|error| map_vfs_error(error, cancellation))?;
                    let file = find(&catalog, input.file).ok_or(FirstSliceError::Identity)?;
                    if snapshot.file() != input.file
                        || snapshot.content_hash() != input.content_hash
                        || u64::try_from(snapshot.content().len()).ok() != Some(input.bytes)
                        || file.path_locator.as_ref() != Some(&snapshot.path().to_locator())
                    {
                        return Err(FirstSliceError::DiscoveryDrift);
                    }
                    #[cfg(test)]
                    observe(
                        self,
                        memory,
                        "attempt",
                        Some(input.file),
                        admission.reservation_bytes(),
                        source_allowance,
                        source_bytes,
                        cancellation,
                    )?;
                    let language = supported_source_language(input, &self.analyzers)
                        .ok_or(FirstSliceError::Identity)?;
                    let parser = self
                        .structural_parsers
                        .get(language)
                        .ok_or(FirstSliceError::Adapter)?;
                    let limits =
                        provider_analysis_limits(&self.analysis_limits, parser.capabilities())?;
                    let request = structural_analysis_request(
                        document.repository,
                        document.generation,
                        &snapshot,
                        matches!(input.class, InputClass::Generated),
                        language,
                        &input.encoding,
                        &limits,
                    )?;
                    let analyzer = self
                        .analyzers
                        .get(language)
                        .ok_or(FirstSliceError::Adapter)?;
                    let analyzed = analyzer.analyze_and_capture(
                        &request,
                        self.extensions.clone(),
                        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                        cancellation,
                    );
                    let fragment = match analyzed {
                        Ok((output, artifact)) => {
                            parsed_files =
                                parsed_files.checked_add(1).ok_or(FirstSliceError::Limits)?;
                            drop(artifact);
                            output.into_parts().0
                        }
                        Err(error) => {
                            let failure = map_adapter_error(error, cancellation);
                            if capacity_refusal(&failure) {
                                continue;
                            }
                            return Err(failure);
                        }
                    };
                    // The incomplete repository cannot prove cross-file candidate sets.
                    // Preserve the producer's exact definitions and written references.
                    if let Err(error) =
                        admission.admit_document(&fragment, memory, true, cancellation)
                    {
                        #[cfg(test)]
                        observe(
                            self,
                            memory,
                            "document-refused",
                            Some(input.file),
                            admission.reservation_bytes(),
                            source_allowance,
                            source_bytes,
                            cancellation,
                        )?;
                        if capacity_refusal(&error) {
                            return Ok(false);
                        }
                        return Err(error);
                    }
                    let result = append_normalized_document_with_metadata_admission(
                        document,
                        fragment,
                        self.analysis_limits.ir(),
                        &mut append,
                        cancellation,
                        Some(&mut |bytes| {
                            admission.admit_known_bytes(bytes, memory, true, cancellation)
                        }),
                    );
                    match result {
                        Ok(()) => {
                            promoted.insert(input.file);
                        }
                        Err(error) if capacity_refusal(&error) => {}
                        Err(error) => return Err(error),
                    }
                    #[cfg(test)]
                    observe(
                        self,
                        memory,
                        "append",
                        Some(input.file),
                        admission.reservation_bytes(),
                        source_allowance,
                        source_bytes,
                        cancellation,
                    )?;
                }
                Ok(true)
            })();
            *reserved_bytes = (*reserved_bytes).max(admission.reservation_bytes());
            memory.release_temporary(
                admission
                    .reservation_bytes()
                    .checked_sub(source_allowance)
                    .ok_or(FirstSliceError::Limits)?,
            )?;
            #[cfg(test)]
            observe(
                self,
                memory,
                "release",
                None,
                admission.reservation_bytes(),
                source_allowance,
                0,
                cancellation,
            )?;
            if !outcome? {
                break;
            }
        }
        check_cancellation(cancellation)?;
        catalog.retain(|entry| !promoted.contains(&entry.file().id));
        check_cancellation(cancellation)?;
        Ok(NativeBatch {
            catalog,
            parsed_files,
        })
    }
}

fn find(entries: &[SourceFileCatalogEntry], file: FileId) -> Option<&FileRecord> {
    entries
        .binary_search_by_key(&file, |entry| entry.file().id)
        .ok()
        .and_then(|index| entries.get(index))
        .map(SourceFileCatalogEntry::file)
}

fn capacity_refusal(error: &FirstSliceError) -> bool {
    matches!(
        error,
        FirstSliceError::ResourceLimit { .. }
            | FirstSliceError::ResourceUnavailable { .. }
            | FirstSliceError::GenerationMemoryLimit { .. }
            | FirstSliceError::AggregateCapacity(_)
    )
}

/// Identifies catalog members and the fallback anchor without masking native file coverage.
pub(super) fn source_only(
    snapshot: &rootlight_storage::GenerationSnapshot,
    file: &FileRecord,
) -> bool {
    // Snapshot construction canonicalizes provenance by identity.
    let provenance = &snapshot.document().provenance;
    snapshot.source_files().find(file.id).is_some()
        || provenance
            .binary_search_by_key(&file.provenance, |record| record.id)
            .ok()
            .and_then(|index| provenance.get(index))
            .is_some_and(|record| record.producer.name() == "rootlight-source-file-fallback")
}
