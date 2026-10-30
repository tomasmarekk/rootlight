//! Retain one admitted native batch beside the durable exact-source catalog.
//! Unselected or capacity-refused files keep their original source-only records;
//! this bounded prefix does not establish repository-wide resolution completeness.

use super::*;

/// Native work completed for one bounded attempt and its unpromoted source entries.
/// Only accepted fragments leave the entries; the remainder retains unknown analysis.
pub(super) struct NativeBatch {
    pub(super) catalog: Vec<SourceFileCatalogEntry>,
    pub(super) parsed_files: u64,
}

impl FirstSliceService {
    /// Promotes only verified native fragments whose complete append fits the current limits.
    /// Capacity refusals retain the exact catalog entry; other failures abort preparation.
    #[expect(
        clippy::too_many_arguments,
        reason = "the batch shares the existing source, output and memory admission owners"
    )]
    pub(super) fn append_fallback_native_batch(
        &self,
        root: &RepositoryRoot,
        inputs: &[ManifestInput],
        document: &mut NormalizedIrDocument,
        mut catalog: Vec<SourceFileCatalogEntry>,
        memory: &mut GenerationMemoryReservation,
        reserved_bytes: &mut u64,
        cancellation: &Cancellation,
    ) -> Result<NativeBatch, FirstSliceError> {
        let maximum_files = self
            .analysis_limits
            .ir()
            .max_files
            .saturating_sub(document.files.len())
            .min(MAX_SNAPSHOT_BATCH_FILES);
        let maximum_bytes = MAX_SNAPSHOT_BATCH_BYTES.min(
            u64::try_from(self.source_snapshots.maximum_bytes)
                .map_err(|_| FirstSliceError::Limits)?,
        );
        let maximum_file_bytes = DiscoveryLimits::from_config(&self.config).max_file_bytes;
        let mut selected = Vec::new();
        let mut requests = Vec::new();
        selected
            .try_reserve_exact(maximum_files)
            .map_err(|_| FirstSliceError::Limits)?;
        requests
            .try_reserve_exact(maximum_files)
            .map_err(|_| FirstSliceError::Limits)?;
        let mut source_bytes = 0_u64;
        for input in inputs {
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
                continue;
            }
            let Some(next) = source_bytes.checked_add(input.bytes.max(1)) else {
                continue;
            };
            if next > maximum_bytes {
                continue;
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
        }
        let mut parsed_files = 0_u64;
        if selected.is_empty() {
            return Ok(NativeBatch {
                catalog,
                parsed_files,
            });
        }
        let source_floor = source_bytes
            .checked_mul(GENERATION_MEMORY_SOURCE_PREFLIGHT_FACTOR)
            .and_then(|bytes| reserved_bytes.checked_add(bytes))
            .ok_or(FirstSliceError::Limits)?;
        if let Err(error) = memory.resize(source_floor, PendingGenerationMemory::Staged, true) {
            if capacity_refusal(&error) {
                return Ok(NativeBatch {
                    catalog,
                    parsed_files,
                });
            }
            return Err(error);
        }
        *reserved_bytes = (*reserved_bytes).max(source_floor);
        let snapshots = root
            .snapshot_batch_with_cancellation(&requests, cancellation)
            .map_err(|error| map_vfs_error(error, cancellation))?;
        if snapshots.len() != selected.len() {
            return Err(FirstSliceError::DiscoveryDrift);
        }
        let mut admission = generation_admission::GenerationAdmission::new(source_floor)?;
        admission.admit_document(document, memory, true, cancellation)?;
        let mut append = DocumentAppendState::from_document(document)?;
        append.aggregate_recovery = true;
        let mut promoted = BTreeSet::new();
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
            let language = supported_source_language(input, &self.analyzers)
                .ok_or(FirstSliceError::Identity)?;
            let parser = self
                .structural_parsers
                .get(language)
                .ok_or(FirstSliceError::Adapter)?;
            let limits = provider_analysis_limits(&self.analysis_limits, parser.capabilities())?;
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
                    parsed_files = parsed_files.checked_add(1).ok_or(FirstSliceError::Limits)?;
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
            if let Err(error) = admission.admit_document(&fragment, memory, true, cancellation) {
                if capacity_refusal(&error) {
                    break;
                }
                return Err(error);
            }
            let result = append_normalized_document_with_metadata_admission(
                document,
                fragment,
                self.analysis_limits.ir(),
                &mut append,
                cancellation,
                Some(&mut |bytes| admission.admit_known_bytes(bytes, memory, true, cancellation)),
            );
            match result {
                Ok(()) => {
                    promoted.insert(input.file);
                }
                Err(error) if capacity_refusal(&error) => {}
                Err(error) => return Err(error),
            }
        }
        *reserved_bytes = (*reserved_bytes).max(admission.reservation_bytes());
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
            | FirstSliceError::AggregateCapacity(_)
    )
}

/// Identifies catalog members and the fallback anchor without masking native file coverage.
pub(super) fn source_only(
    snapshot: &rootlight_storage::GenerationSnapshot,
    file: &FileRecord,
) -> bool {
    snapshot.source_files().find(file.id).is_some()
        || snapshot.document().provenance.iter().any(|provenance| {
            provenance.id == file.provenance
                && provenance.producer.name() == "rootlight-source-file-fallback"
        })
}
