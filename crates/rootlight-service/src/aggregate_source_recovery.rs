//! Preparation-owned immutable source union and genuine aggregate rejection.
//! The frame outlives abandoned structural owners, while the private witness
//! cannot be synthesized from a public resource error or an adapter message.

use super::*;
use rootlight_ir::{
    AggregateCapacityResource as Resource, AggregateCapacityStage as Stage, AggregateSourceRecovery,
};

/// Source-free evidence created only at a checked combined capacity boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggregateCapacityFailure {
    stage: Stage,
    resource: Resource,
    observed: u64,
    limit: u64,
}

impl AggregateCapacityFailure {
    pub(super) fn is_structural_append(self) -> bool {
        self.stage == Stage::StructuralAppend
    }

    pub(super) fn source_disposition_detail(self) -> Result<String, FirstSliceError> {
        if !self.is_structural_append() {
            return Err(FirstSliceError::Identity);
        }
        let resource = format!("{:?}", self.resource);
        if resource.len() > 16 {
            return Err(FirstSliceError::Identity);
        }
        // Every real disposition has the exact byte width admitted by its
        // private source budget model, including actual unsigned counters.
        Ok(format!(
            "stage=structural_append; resource={resource:<16}; observed={:020}; limit={:020}",
            self.observed, self.limit
        ))
    }

    pub(super) fn from_combined_usage(error: &IrDocumentUsageError) -> Option<Self> {
        let (resource, observed, limit) = match error {
            IrDocumentUsageError::Validation(error) => match *error {
                IrDocumentValidationError::CollectionLimit {
                    collection,
                    observed,
                    limit,
                } => {
                    let resource = match collection {
                        "files" => Resource::Files,
                        "entities" => Resource::Entities,
                        "occurrences" => Resource::Occurrences,
                        "relations" => Resource::Relations,
                        "provenance" => Resource::Provenance,
                        "source_mappings" => Resource::SourceMappings,
                        "coverage_records" => Resource::Coverage,
                        "skipped_regions" => Resource::SkippedRegions,
                        "diagnostics" => Resource::Diagnostics,
                        "extensions" => Resource::Extensions,
                        _ => return None,
                    };
                    (resource, observed, limit)
                }
                IrDocumentValidationError::TotalRecordLimit { observed, limit } => {
                    (Resource::Records, observed, limit)
                }
                IrDocumentValidationError::TotalNestedItemLimit { observed, limit } => {
                    (Resource::NestedItems, observed, limit)
                }
                IrDocumentValidationError::TotalStringLimit { observed, limit } => {
                    (Resource::StringBytes, observed, limit)
                }
                IrDocumentValidationError::TotalExtensionBytesLimit { observed, limit } => {
                    (Resource::ExtensionBytes, observed, limit)
                }
                IrDocumentValidationError::TotalDiagnosticBytesLimit { observed, limit } => {
                    (Resource::DiagnosticBytes, observed, limit)
                }
                _ => return None,
            },
            IrDocumentUsageError::AccountingOverflow | IrDocumentUsageError::Interrupted => {
                return None;
            }
        };
        let observed = u64::try_from(observed).ok()?;
        let limit = u64::try_from(limit).ok()?;
        (observed > limit && observed != u64::MAX).then_some(Self {
            stage: Stage::StructuralAppend,
            resource,
            observed,
            limit,
        })
    }

    pub(super) fn from_generation_text(error: &IdentityVerificationError) -> Option<Self> {
        let IdentityVerificationError::Control(
            rootlight_storage::GenerationControlError::BudgetExceeded {
                resource,
                observed,
                limit,
            },
        ) = error
        else {
            return None;
        };
        let resource = match resource {
            rootlight_storage::GenerationResource::TextBytes => Resource::GenerationTextBytes,
            rootlight_storage::GenerationResource::EncodedTextBytes => {
                Resource::GenerationEncodedTextBytes
            }
            _ => return None,
        };
        (*observed > *limit && *observed != u64::MAX).then_some(Self {
            stage: Stage::GenerationText,
            resource,
            observed: *observed,
            limit: *limit,
        })
    }

    pub(super) fn bind(
        self,
        manifest: ContentHash,
        members: usize,
        derived_members: u64,
        text_snapshot_bytes: u64,
    ) -> Result<AggregateSourceRecovery, FirstSliceError> {
        let value = AggregateSourceRecovery {
            source_inventory: manifest,
            members: u64::try_from(members).map_err(|_| FirstSliceError::Limits)?,
            derived_members,
            stage: self.stage,
            resource: self.resource,
            observed: self.observed,
            limit: self.limit,
            text_snapshot_bytes,
        };
        value
            .is_valid()
            .then_some(value)
            .ok_or(FirstSliceError::Identity)
    }
}

/// Private checked cause for an intrinsically oversized complete oracle restore.
/// It is separate from aggregate IR/text witnesses and never accepts a generic error.
#[derive(Debug, Clone, Copy)]
pub(super) struct DurableRestoreCapacity {
    observed: u64,
    limit: u64,
}

impl DurableRestoreCapacity {
    pub(super) fn from_sealed_plan(observed: u64) -> Option<Self> {
        let limit = durable::oracle_restore_memory_limit();
        (observed > limit && observed != u64::MAX).then_some(Self { observed, limit })
    }

    pub(super) fn error(self) -> FirstSliceError {
        let mut error =
            generation_memory_limit(0, self.observed, PendingGenerationMemory::Reserved);
        if let FirstSliceError::GenerationMemoryLimit { limit, .. } = &mut error {
            *limit = self.limit;
        }
        error
    }

    pub(super) fn matches(self, error: &FirstSliceError) -> bool {
        matches!(error, FirstSliceError::GenerationMemoryLimit { observed, limit, .. }
            if *observed == self.observed && *limit == self.limit)
    }

    pub(super) fn annotate(
        self,
        document: &mut NormalizedIrDocument,
    ) -> Result<(), FirstSliceError> {
        let diagnostic = document
            .diagnostics
            .iter_mut()
            .find(|record| record.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE)
            .ok_or(FirstSliceError::Identity)?;
        diagnostic.message = format!(
            "complete semantic oracle restore requires {} modeled bytes above limit {}; exact source retrieval remains available with unknown structural and semantic coverage",
            self.observed, self.limit,
        );
        diagnostic.id =
            derive_diagnostic_record_id(diagnostic).map_err(|_| FirstSliceError::Identity)?;
        Ok(())
    }
}

pub(super) struct PreparedSourceFrame {
    pub(super) inputs: Vec<ManifestInput>,
    pub(super) claims: Vec<FileIdentityClaim>,
    pub(super) snapshots: BTreeMap<FileId, SourceSnapshot>,
    pub(super) captured: Option<Arc<source_owner::CapturedGenerationSources>>,
    pub(super) derived_fingerprints: Vec<InputFingerprint>,
    pub(super) derived_files: u64,
    pub(super) origins: Vec<(FileId, FileId)>,
}

// Eligible preparation keeps the original snapshot map as a fallback owner.
// Its raw Arc buffers are already in source preflight; charge the additional
// retained map/value/path metadata before constructing analysis clones.
pub(super) fn ordinary_snapshot_metadata_bytes(
    snapshots: &BTreeMap<FileId, SourceSnapshot>,
) -> Result<u64, FirstSliceError> {
    snapshots.values().try_fold(0_u64, |total, snapshot| {
        let metadata = size_of::<SourceSnapshot>()
            .checked_add(
                snapshot
                    .path()
                    .retained_memory_bytes()
                    .ok_or(FirstSliceError::Limits)?,
            )
            .and_then(|bytes| bytes.checked_mul(2))
            .and_then(|bytes| bytes.checked_add(SOURCE_RETENTION_BTREE_ENTRY_OVERHEAD))
            .ok_or(FirstSliceError::Limits)?;
        total
            .checked_add(u64::try_from(metadata).map_err(|_| FirstSliceError::Limits)?)
            .ok_or(FirstSliceError::Limits)
    })
}

// The plan counts metadata while borrowing existing preparation owners. No
// recovery frame, manifest clone or validation set exists before admission.
#[expect(
    clippy::too_many_arguments,
    reason = "preflight measures distinct owner capacities before constructing a recovery frame"
)]
fn measure_frame_memory<'a>(
    inputs: impl Iterator<Item = &'a ManifestInput> + Clone,
    input_capacity: usize,
    claims: &Vec<FileIdentityClaim>,
    snapshots: &BTreeMap<FileId, SourceSnapshot>,
    captured: Option<&Arc<source_owner::CapturedGenerationSources>>,
    fingerprint_capacity: usize,
    origin_capacity: usize,
    include_snapshots: bool,
) -> Result<u64, FirstSliceError> {
    fn add(total: &mut usize, bytes: usize) -> Result<(), FirstSliceError> {
        *total = total.checked_add(bytes).ok_or(FirstSliceError::Limits)?;
        Ok(())
    }
    fn vector<T>(total: &mut usize, values: &Vec<T>) -> Result<(), FirstSliceError> {
        add(
            total,
            values
                .capacity()
                .checked_mul(size_of::<T>())
                .ok_or(FirstSliceError::Limits)?,
        )?;
        add(total, SOURCE_RETENTION_ALLOCATION_OVERHEAD)
    }
    let mut bytes = size_of::<PreparedSourceFrame>();
    add(
        &mut bytes,
        input_capacity
            .checked_mul(size_of::<ManifestInput>())
            .ok_or(FirstSliceError::Limits)?,
    )?;
    add(&mut bytes, SOURCE_RETENTION_ALLOCATION_OVERHEAD)?;
    vector(&mut bytes, claims)?;
    add(
        &mut bytes,
        fingerprint_capacity
            .checked_mul(size_of::<InputFingerprint>())
            .ok_or(FirstSliceError::Limits)?,
    )?;
    add(&mut bytes, SOURCE_RETENTION_ALLOCATION_OVERHEAD)?;
    add(
        &mut bytes,
        origin_capacity
            .checked_mul(size_of::<(FileId, FileId)>())
            .ok_or(FirstSliceError::Limits)?,
    )?;
    add(&mut bytes, SOURCE_RETENTION_ALLOCATION_OVERHEAD)?;
    for input in inputs.clone() {
        add(&mut bytes, input.path.capacity())?;
        add(&mut bytes, input.encoding.capacity())?;
        vector(&mut bytes, &input.language_signals)?;
        for signal in &input.language_signals {
            add(&mut bytes, signal.language.capacity())?;
        }
        vector(&mut bytes, &input.project_language_witnesses)?;
        for witness in &input.project_language_witnesses {
            add(&mut bytes, witness.language.capacity())?;
            add(&mut bytes, witness.project_path.capacity())?;
        }
        if let Some(rule) = &input.decisive_rule {
            add(&mut bytes, rule.source.capacity())?;
            add(&mut bytes, rule.pattern.capacity())?;
        }
    }
    for claim in claims {
        add(&mut bytes, claim.path.capacity())?;
        vector(&mut bytes, &claim.path_identity)?;
    }
    if include_snapshots {
        for snapshot in snapshots.values() {
            add(&mut bytes, unique_source_snapshot_bytes(snapshot)?)?;
        }
    }
    if let Some(captured) = captured {
        add(
            &mut bytes,
            size_of::<source_owner::CapturedGenerationSources>(),
        )?;
        for (_, source) in captured.files() {
            add(&mut bytes, size_of::<source_owner::CapturedSourceFile>())?;
            add(&mut bytes, SOURCE_RETENTION_BTREE_ENTRY_OVERHEAD)?;
            add(&mut bytes, source.private_name.capacity())?;
            add(
                &mut bytes,
                source
                    .capture
                    .path()
                    .retained_memory_bytes()
                    .ok_or(FirstSliceError::Limits)?,
            )?;
        }
    }
    // Include allocator headers for the bounded per-member strings above.
    let headers = inputs
        .clone()
        .try_fold(0usize, |total, input| {
            total
                .checked_add(8)?
                .checked_add(input.language_signals.len())?
                .checked_add(input.project_language_witnesses.len().checked_mul(2)?)
        })
        .ok_or(FirstSliceError::Limits)?;
    add(
        &mut bytes,
        headers
            .checked_mul(SOURCE_RETENTION_ALLOCATION_OVERHEAD)
            .ok_or(FirstSliceError::Limits)?,
    )?;
    u64::try_from(bytes).map_err(|_| FirstSliceError::Limits)
}

#[cfg(test)]
thread_local! { pub(super) static FRAME_SEALS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

impl PreparedSourceFrame {
    pub(super) fn retained_memory_bytes(&self) -> Result<u64, FirstSliceError> {
        measure_frame_memory(
            self.inputs.iter(),
            self.inputs.capacity(),
            &self.claims,
            &self.snapshots,
            self.captured.as_ref(),
            self.derived_fingerprints.capacity(),
            self.origins.capacity(),
            true,
        )
    }

    pub(super) fn planned_memory_bytes(
        originals: &[ManifestInput],
        derived: &container_sources::PreparedSources,
        claims: &Vec<FileIdentityClaim>,
        snapshots: &BTreeMap<FileId, SourceSnapshot>,
        captured: Option<&Arc<source_owner::CapturedGenerationSources>>,
    ) -> Result<u64, FirstSliceError> {
        let count = originals
            .len()
            .checked_add(derived.inputs.len())
            .ok_or(FirstSliceError::Limits)?;
        let slots = count.checked_mul(2).ok_or(FirstSliceError::Limits)?;
        let inputs = originals.iter().chain(&derived.inputs);
        let frame = measure_frame_memory(
            inputs.clone(),
            slots,
            claims,
            snapshots,
            captured,
            derived.fingerprints.capacity(),
            derived.source_origins().len(),
            true,
        )?;
        // The original metadata survives until the sealed union replaces it.
        // Raw Arc/captured bodies are charged once; only metadata is doubled.
        let metadata = measure_frame_memory(
            inputs,
            slots,
            claims,
            snapshots,
            captured,
            derived.fingerprints.capacity(),
            derived.source_origins().len(),
            false,
        )?;
        let validation = u64::try_from(count)
            .map_err(|_| FirstSliceError::Limits)?
            .checked_mul(
                u64::try_from(
                    2 * SOURCE_RETENTION_BTREE_ENTRY_OVERHEAD
                        + 2 * size_of::<FileId>()
                        + size_of::<&str>(),
                )
                .map_err(|_| FirstSliceError::Limits)?,
            )
            .ok_or(FirstSliceError::Limits)?;
        frame
            .checked_add(metadata)
            .and_then(|bytes| bytes.checked_add(validation))
            .ok_or(FirstSliceError::Limits)
    }

    pub(super) fn seal(
        originals: &[ManifestInput],
        derived: &container_sources::PreparedSources,
        claims: Vec<FileIdentityClaim>,
        snapshots: BTreeMap<FileId, SourceSnapshot>,
        captured: Option<Arc<source_owner::CapturedGenerationSources>>,
        cancellation: &Cancellation,
    ) -> Result<Self, FirstSliceError> {
        check_cancellation(cancellation)?;
        #[cfg(test)]
        FRAME_SEALS
            .with(|count| count.set(count.get().checked_add(1).expect("test seal count fits")));
        let count = originals
            .len()
            .checked_add(derived.inputs.len())
            .ok_or(FirstSliceError::Limits)?;
        if count != claims.len() || count == 0 {
            return Err(FirstSliceError::Identity);
        }
        let mut inputs = Vec::new();
        inputs
            .try_reserve_exact(count)
            .map_err(|_| FirstSliceError::Limits)?;
        inputs.extend_from_slice(originals);
        inputs.extend_from_slice(&derived.inputs);
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut retained = 0usize;
        for (input, claim) in inputs.iter().zip(&claims) {
            check_cancellation(cancellation)?;
            if !ids.insert(input.file)
                || !paths.insert(input.path.as_str())
                || claim.file != input.file
                || claim.derived_file() != input.file
                || claim.path != input.path
                || claim.content_hash != input.content_hash
                || claim.byte_length != input.bytes
            {
                return Err(FirstSliceError::Identity);
            }
            let streamed = captured
                .as_ref()
                .is_some_and(|sources| sources.contains(input.file));
            match (snapshots.get(&input.file), streamed) {
                (Some(snapshot), false)
                    if snapshot.file() == input.file
                        && snapshot.path().as_str() == input.path
                        && snapshot.content_hash() == input.content_hash
                        && u64::try_from(snapshot.content().len()).ok() == Some(input.bytes) =>
                {
                    retained = retained.checked_add(1).ok_or(FirstSliceError::Limits)?;
                }
                (None, true) => {}
                _ => return Err(FirstSliceError::DiscoveryDrift),
            }
        }
        if retained != snapshots.len() {
            return Err(FirstSliceError::Identity);
        }
        let origins = derived.source_origins().to_vec();
        let derived_ids = derived
            .inputs
            .iter()
            .map(|input| input.file)
            .collect::<BTreeSet<_>>();
        let mut originated = BTreeSet::new();
        for &(file, container) in &origins {
            if !derived_ids.contains(&file)
                || !ids.contains(&container)
                || derived_ids.contains(&container)
                || !originated.insert(file)
            {
                return Err(FirstSliceError::Identity);
            }
        }
        if originated != derived_ids {
            return Err(FirstSliceError::Identity);
        }
        Ok(Self {
            inputs,
            claims,
            snapshots,
            captured,
            derived_fingerprints: derived.fingerprints.clone(),
            origins,
            derived_files: u64::try_from(derived.inputs.len())
                .map_err(|_| FirstSliceError::Limits)?,
        })
    }
}

/// Whole-source lexical projection remains separate from analysis coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstSliceTextSearchCoverage {
    /// Whole source participated in projection; vocabulary omission bounds remain.
    WholeSourceProjection,
    /// Captured source bytes are exact but only file metadata was indexed.
    MetadataOnlyCaptured,
    /// Raw bytes have no supported lossless text projection.
    UnsupportedEncoding,
}

pub(super) fn text_search_coverage(
    encoding: &str,
    byte_length: u64,
    snapshot_bytes: u64,
) -> FirstSliceTextSearchCoverage {
    if !matches!(encoding, "utf-8" | "utf-16le" | "utf-16be") {
        FirstSliceTextSearchCoverage::UnsupportedEncoding
    } else if byte_length > snapshot_bytes {
        FirstSliceTextSearchCoverage::MetadataOnlyCaptured
    } else {
        FirstSliceTextSearchCoverage::WholeSourceProjection
    }
}

/// Exact universal cause plus the selected file's independent text-search scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceSourceRecovery {
    /// Authenticated complete-inventory aggregate explanation.
    pub cause: AggregateSourceRecovery,
    /// What source text participated in lexical indexing for this file.
    pub text_search: FirstSliceTextSearchCoverage,
}

impl FirstSliceService {
    /// Returns a generation-authenticated aggregate cause for one exact member.
    ///
    /// `None` includes legacy generic file-only generations and ordinary analysis.
    /// Whole-source projection does not waive vocabulary or per-query budgets.
    ///
    /// # Errors
    /// Rejects unknown file/generation, cancellation, missing or altered durable
    /// evidence, and any mismatch between the universal cause and member set.
    pub fn source_file_recovery_until(
        &self,
        generation: GenerationId,
        file: FileId,
        cancellation: &Cancellation,
    ) -> Result<Option<FirstSliceSourceRecovery>, FirstSliceError> {
        if let Some(lease) = self.source_only_lease(generation, cancellation)? {
            let record = lease
                .authority
                .find_file(file)
                .ok_or(FirstSliceError::Query)?;
            return Ok(lease
                .authority
                .recovery()
                .map(|cause| FirstSliceSourceRecovery {
                    cause,
                    text_search: text_search_coverage(
                        &record.encoding,
                        record.byte_length,
                        cause.text_snapshot_bytes,
                    ),
                }));
        }
        let lease = self.generation_lease(generation, cancellation)?;
        let snapshot = lease.generation();
        let record = snapshot.find_file(file).ok_or(FirstSliceError::Query)?;
        let mut values = snapshot.document().extensions.iter().filter(|extension| {
            extension.namespace == rootlight_ir::AGGREGATE_SOURCE_RECOVERY_NAMESPACE
        });
        let Some(envelope) = values.next() else {
            return Ok(None);
        };
        if values.next().is_some() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let cause = rootlight_ir::decode_aggregate_source_recovery_envelope(envelope)
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        if cause.source_inventory != snapshot.metadata().manifest_hash()
            || u64::try_from(snapshot.file_count()).ok() != Some(cause.members)
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let text_search = text_search_coverage(
            &record.encoding,
            record.byte_length,
            cause.text_snapshot_bytes,
        );
        Ok(Some(FirstSliceSourceRecovery { cause, text_search }))
    }
}

pub(super) fn policy_hash(
    limits: &IrLimits,
    budget: GenerationBudget,
) -> Result<ContentHash, FirstSliceError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(rootlight_ir::AGGREGATE_SOURCE_RECOVERY_REVISION);
    for value in [
        limits.max_document_bytes,
        limits.max_extension_envelope_bytes,
        limits.max_files,
        limits.max_entities,
        limits.max_occurrences,
        limits.max_relations,
        limits.max_provenance_records,
        limits.max_source_mappings,
        limits.max_coverage_records,
        limits.max_skipped_regions,
        limits.max_diagnostics,
        limits.max_extensions,
        limits.max_total_records,
        limits.max_nested_items_per_record,
        limits.max_total_nested_items,
        limits.max_string_bytes,
        limits.max_total_string_bytes,
        limits.max_extension_payload_bytes,
        limits.max_total_extension_bytes,
        limits.max_diagnostic_message_bytes,
        limits.max_total_diagnostic_bytes,
    ] {
        hasher.update(
            &u64::try_from(value)
                .map_err(|_| FirstSliceError::Limits)?
                .to_be_bytes(),
        );
    }
    for resource in [
        rootlight_storage::GenerationResource::Rows,
        rootlight_storage::GenerationResource::SourceReferences,
        rootlight_storage::GenerationResource::TextBytes,
        rootlight_storage::GenerationResource::EncodedTextBytes,
    ] {
        hasher.update(&budget.limit(resource).to_be_bytes());
    }
    Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}

pub(super) fn attach_cause_and_origins(
    document: &mut NormalizedIrDocument,
    catalog: &SourceFileCatalog,
    frame: &PreparedSourceFrame,
    cause: AggregateSourceRecovery,
    cancellation: &Cancellation,
) -> Result<(), FirstSliceError> {
    check_cancellation(cancellation)?;
    if !cause.is_valid()
        || usize::try_from(cause.members).ok() != Some(frame.inputs.len())
        || usize::try_from(cause.derived_members).ok() != Some(frame.origins.len())
        || document.files.len() != 1
        || !document.entities.is_empty()
        || !document.occurrences.is_empty()
        || !document.relations.is_empty()
    {
        return Err(FirstSliceError::Identity);
    }
    let anchor = &document.files[0];
    let provenance = anchor.provenance;
    let source = anchor
        .evidence
        .source
        .clone()
        .ok_or(FirstSliceError::Identity)?;
    let mut extensions = Vec::new();
    extensions
        .try_reserve_exact(
            frame
                .origins
                .len()
                .checked_add(1)
                .ok_or(FirstSliceError::Limits)?,
        )
        .map_err(|_| FirstSliceError::Limits)?;
    extensions.push(
        rootlight_ir::new_aggregate_source_recovery_envelope(
            document.repository,
            document.generation,
            provenance,
            source.clone(),
            cause,
        )
        .map_err(|_| FirstSliceError::Identity)?,
    );
    let reference = |file| -> Result<SourceRef, FirstSliceError> {
        let record = if anchor.id == file {
            Some(anchor)
        } else {
            catalog.find(file)
        }
        .ok_or(FirstSliceError::Identity)?;
        record
            .evidence
            .source
            .clone()
            .ok_or(FirstSliceError::Identity)
    };
    for &(file, container) in &frame.origins {
        check_cancellation(cancellation)?;
        extensions.push(
            rootlight_ir::new_aggregate_source_origin_envelope(
                document.repository,
                document.generation,
                provenance,
                source.clone(),
                reference(file)?,
                reference(container)?,
            )
            .map_err(|_| FirstSliceError::Identity)?,
        );
    }
    document
        .extensions
        .try_reserve_exact(extensions.len())
        .map_err(|_| FirstSliceError::Limits)?;
    document.extensions.extend(extensions);
    let extension_count =
        u64::try_from(document.extensions.len()).map_err(|_| FirstSliceError::Limits)?;
    let coverage = document
        .coverage_records
        .iter_mut()
        .find(|coverage| coverage.domain == IrFactDomain::Extensions)
        .ok_or(FirstSliceError::Identity)?;
    coverage.discovered = extension_count;
    coverage.indexed = extension_count;
    coverage.id = derive_coverage_record_id(coverage).map_err(|_| FirstSliceError::Identity)?;
    let diagnostic = document
        .diagnostics
        .iter_mut()
        .find(|diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE)
        .ok_or(FirstSliceError::Identity)?;
    diagnostic.message = "aggregate structural capacity exceeded - exact source bytes retained; analysis unknown; lexical scope reported per file".to_owned();
    diagnostic.id =
        derive_diagnostic_record_id(diagnostic).map_err(|_| FirstSliceError::Identity)?;
    let mut marker = diagnostic.clone();
    marker.code = rootlight_ir::AGGREGATE_SOURCE_RECOVERY_DIAGNOSTIC.to_owned();
    marker.id = derive_diagnostic_record_id(&marker).map_err(|_| FirstSliceError::Identity)?;
    document
        .diagnostics
        .try_reserve_exact(1)
        .map_err(|_| FirstSliceError::Limits)?;
    document.diagnostics.push(marker);
    let coverage = document
        .coverage_records
        .iter_mut()
        .find(|coverage| coverage.domain == IrFactDomain::Diagnostics)
        .ok_or(FirstSliceError::Identity)?;
    coverage.discovered = 2;
    coverage.indexed = 2;
    coverage.id = derive_coverage_record_id(coverage).map_err(|_| FirstSliceError::Identity)?;
    Ok(())
}
