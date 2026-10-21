//! Reserve exact source-disposition metadata before retaining ordinary file facts.
//! Only a host-measured combined append limit can replace an already
//! admitted single-file physical fragment without origin mappings. Generated
//! classification is preserved; provider failures never enter this decision.

use super::*;

const EXPLANATION_BYTES: usize = 109;

// Only these two metadata collections can gain new IDs during append planning.
// Retain their original witnesses without retaining or decoding a second document.
pub(super) struct RawMetadataWitness {
    ids: Vec<FactId>,
}

impl RawMetadataWitness {
    pub(super) fn capture(
        document: &NormalizedIrDocument,
        cancellation: &Cancellation,
        admission: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<Self, FirstSliceError> {
        check_cancellation(cancellation)?;
        let count = document
            .coverage_records
            .len()
            .checked_add(document.extensions.len())
            .ok_or(FirstSliceError::Limits)?;
        let bytes = count
            .checked_mul(std::mem::size_of::<FactId>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(FirstSliceError::Limits)?;
        admission(bytes)?;
        let mut ids = Vec::new();
        ids.try_reserve_exact(count)
            .map_err(|_| FirstSliceError::ResourceUnavailable {
                resource: FirstSliceResource::MemoryBytes,
            })?;
        let actual = ids
            .capacity()
            .checked_mul(std::mem::size_of::<FactId>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(FirstSliceError::Limits)?;
        if actual > bytes {
            admission(actual - bytes)?;
        }
        for id in document
            .coverage_records
            .iter()
            .map(|record| record.id)
            .chain(document.extensions.iter().map(|record| record.id))
        {
            check_cancellation(cancellation)?;
            ids.push(id);
        }
        check_cancellation(cancellation)?;
        ids.sort_unstable();
        check_cancellation(cancellation)?;
        ids.dedup();
        Ok(Self { ids })
    }

    pub(super) fn contains(&self, id: FactId) -> bool {
        self.ids.binary_search(&id).is_ok()
    }
}

struct Entry {
    file: FileId,
    minimum: Option<IrDocumentUsage>,
    degradable: bool,
}

pub(super) struct Reservation {
    entries: Vec<Entry>,
    next: usize,
    remaining: IrDocumentUsage,
    unavailable: bool,
}

fn standalone(input: &RustSourceInput) -> bool {
    // The service restricts this table to physical Structural inputs. A generated
    // classification alone adds no origin mapping and remains exact metadata.
    input.origins.is_empty()
}

fn disposition(
    repository: RepositoryId,
    generation: GenerationId,
    input: &RustSourceInput,
    language: &str,
    explanation: &str,
    cancellation: &Cancellation,
) -> Result<NormalizedIrDocument, FirstSliceError> {
    check_cancellation(cancellation)?;
    if !standalone(input) || explanation.len() > EXPLANATION_BYTES {
        return Err(FirstSliceError::Identity);
    }
    let path = input.snapshot.path();
    let mut document = unsupported_source_document(
        repository,
        generation,
        &UnsupportedSourceInput {
            claim: FileIdentityClaim {
                file: input.snapshot.file(),
                repository,
                path: fallible_copy_string(path.as_str())?,
                path_identity: fallible_copy_bytes(path.identity_bytes())?,
                content_hash: input.snapshot.content_hash(),
                byte_length: input.snapshot.metadata().length,
            },
            language: fallible_copy_string(language)?,
            encoding: fallible_copy_string(&input.encoding)?,
            generated: input.generated,
            disposition: UnsupportedSourceDisposition::AggregateFileCapacity,
        },
    )?;
    for gap in &mut document.skipped_regions {
        check_cancellation(cancellation)?;
        gap.detail = format!("aggregate-file-capacity: {explanation}");
        gap.id = derive_skipped_region_id(gap).map_err(|_| FirstSliceError::Identity)?;
    }
    Ok(document)
}

fn checked_combination(
    target: IrDocumentUsage,
    source: IrDocumentUsage,
    remaining: IrDocumentUsage,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<IrDocumentUsage, FirstSliceError> {
    check_cancellation(cancellation)?;
    target
        .checked_add(source, limits)
        .and_then(|usage| usage.checked_add(remaining, limits))
        .map_err(|error| {
            if let Some(cancelled) = current_cancellation(cancellation) {
                cancelled
            } else if let Some(cause) = AggregateCapacityFailure::from_combined_usage(&error) {
                FirstSliceError::AggregateCapacity(cause)
            } else {
                map_ir_usage_error(error, cancellation)
            }
        })
}

impl Reservation {
    #[expect(
        clippy::too_many_arguments,
        reason = "optional reservation preserves the original admission guard"
    )]
    pub(super) fn new(
        repository: RepositoryId,
        generation: GenerationId,
        sources: &[RustSourceInput],
        languages: &BTreeMap<FileId, String>,
        native_unavailable: &BTreeMap<FileId, native_source_admission::NativeSourceRefusal>,
        limits: &IrLimits,
        reserved_bytes: &mut u64,
        memory: &mut GenerationMemoryReservation,
        durable: bool,
        cancellation: &Cancellation,
    ) -> Result<Option<Self>, FirstSliceError> {
        let previous = *reserved_bytes;
        match Self::prepare(
            repository,
            generation,
            sources,
            languages,
            native_unavailable,
            limits,
            reserved_bytes,
            memory,
            durable,
            cancellation,
        ) {
            Ok(value) => Ok(Some(value)),
            Err(
                FirstSliceError::ResourceLimit { .. }
                | FirstSliceError::AggregateCapacity(_)
                | FirstSliceError::GenerationMemoryLimit { .. },
            ) => {
                // Failed optional planning owns no surviving table. Return to
                // the original floor, then run the unchanged strict pipeline.
                check_cancellation(cancellation)?;
                memory.resize(previous, PendingGenerationMemory::Reserved, durable)?;
                *reserved_bytes = previous;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "source identity and its existing admission guard remain explicit"
    )]
    fn prepare(
        repository: RepositoryId,
        generation: GenerationId,
        sources: &[RustSourceInput],
        languages: &BTreeMap<FileId, String>,
        native_unavailable: &BTreeMap<FileId, native_source_admission::NativeSourceRefusal>,
        limits: &IrLimits,
        reserved_bytes: &mut u64,
        memory: &mut GenerationMemoryReservation,
        durable: bool,
        cancellation: &Cancellation,
    ) -> Result<Self, FirstSliceError> {
        check_cancellation(cancellation)?;
        let table_bytes = sources
            .len()
            .checked_mul(size_of::<Entry>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(FirstSliceError::Limits)?;
        let admitted = reserved_bytes
            .checked_add(table_bytes)
            .ok_or(FirstSliceError::Limits)?;
        memory.resize(admitted, PendingGenerationMemory::Staged, durable)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(sources.len())
            .map_err(|_| FirstSliceError::Limits)?;
        let actual = entries
            .capacity()
            .checked_mul(size_of::<Entry>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(FirstSliceError::Limits)?;
        *reserved_bytes = reserved_bytes
            .checked_add(actual)
            .ok_or(FirstSliceError::Limits)?;
        memory.resize(*reserved_bytes, PendingGenerationMemory::Staged, durable)?;
        let mut remaining = IrDocumentUsage::default();
        // This maximum-width explanation is a private budget model, never a
        // published failure or a fabricated AggregateCapacityFailure witness.
        let model_explanation = "x".repeat(EXPLANATION_BYTES);
        for input in sources {
            check_cancellation(cancellation)?;
            let native = native_unavailable.get(&input.snapshot.file());
            let degradable = standalone(input) && native.is_none();
            let minimum = if degradable || native.is_some() {
                let language = languages
                    .get(&input.snapshot.file())
                    .ok_or(FirstSliceError::Identity)?;
                let mut model = if let Some(proof) = native {
                    native_source_admission::source_document(
                        repository,
                        generation,
                        input,
                        language,
                        proof,
                        sources,
                        limits,
                        cancellation,
                    )?
                } else {
                    disposition(
                        repository,
                        generation,
                        input,
                        language,
                        &model_explanation,
                        cancellation,
                    )?
                };
                // The existing append planner may aggregate native warnings.
                // Reserve their mandatory source/gap records, not optional warnings.
                if native.is_some() {
                    model.diagnostics.clear();
                    for coverage in &mut model.coverage_records {
                        if coverage.domain == IrFactDomain::Diagnostics {
                            coverage.status = CoverageStatus::Unknown;
                            coverage.discovered = 0;
                            coverage.indexed = 0;
                            coverage.skipped = 0;
                            coverage.id = derive_coverage_record_id(coverage)
                                .map_err(|_| FirstSliceError::Identity)?;
                        }
                    }
                }
                let usage = measure_append_usage(&model, limits, cancellation)?;
                if native.is_none() {
                    rootlight_ir::validate_ir_document(
                        &model,
                        limits,
                        &ExtensionSupport::default(),
                    )
                    .map_err(|_| FirstSliceError::Identity)?;
                }
                // Existing native dispositions can reference an authenticated
                // origin in another file. Their checked builder owns that join;
                // normal combined validation still verifies the final document.
                check_cancellation(cancellation)?;
                remaining = checked_combination(
                    remaining,
                    usage.clone(),
                    IrDocumentUsage::default(),
                    limits,
                    cancellation,
                )?;
                Some(usage)
            } else {
                None
            };
            entries.push(Entry {
                file: input.snapshot.file(),
                minimum,
                degradable,
            });
        }
        Ok(Self {
            entries,
            next: 0,
            remaining,
            unavailable: false,
        })
    }

    pub(super) fn remaining(&self) -> &IrDocumentUsage {
        &self.remaining
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "selected owned append plans retain exact source and admission ownership"
    )]
    pub(super) fn choose(
        &mut self,
        target: &NormalizedIrDocument,
        state: &DocumentAppendState,
        input: &RustSourceInput,
        language: &str,
        source: NormalizedIrDocument,
        limits: &IrLimits,
        cancellation: &Cancellation,
        admission: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<(PreparedDocumentAppend, bool), FirstSliceError> {
        check_cancellation(cancellation)?;
        let entry = self
            .entries
            .get(self.next)
            .ok_or(FirstSliceError::Identity)?;
        if entry.file != input.snapshot.file()
            || source.repository != target.repository
            || source.generation != target.generation
        {
            return Err(FirstSliceError::Identity);
        }
        let next_remaining = match &entry.minimum {
            Some(minimum) => self
                .remaining
                .clone()
                .checked_sub(minimum.clone())
                .map_err(|error| map_ir_usage_error(error, cancellation))?,
            None => self.remaining.clone(),
        };
        let single = source.files.len() == 1;
        let exact_primary = source.files.first().is_some_and(|file| {
            file.id == input.snapshot.file()
                && file.repository == target.repository
                && file.generation == target.generation
                && file.path == input.snapshot.path().as_str()
                && file.content_hash == input.snapshot.content_hash()
                && file.byte_length == input.snapshot.metadata().length
                && file.language == language
                && file.encoding == input.encoding
                && file.generated == input.generated
                && file.evidence.source.as_ref().is_some_and(|reference| {
                    reference.repository() == target.repository
                        && reference.generation() == target.generation
                        && reference.span().file() == input.snapshot.file()
                        && reference.span().start_byte() == 0
                        && reference.span().end_byte() == input.snapshot.metadata().length
                        && reference.content_hash() == input.snapshot.content_hash()
                })
        });
        if single && !exact_primary {
            // A foreign single-file claim cannot become a source disposition.
            return Err(FirstSliceError::Identity);
        }
        let prepared = prepare_normalized_document_append(
            target,
            source,
            limits,
            state,
            cancellation,
            Some(&mut *admission),
            Some(&next_remaining),
        );
        let (prepared, replaced) = match prepared {
            Ok(prepared) => (prepared, false),
            Err(FirstSliceError::AggregateCapacity(cause)) if entry.degradable && single => {
                if !cause.is_structural_append() {
                    return Err(FirstSliceError::AggregateCapacity(cause));
                }
                // The rejected owned source and all temporary plan vectors have
                // been dropped before constructing or admitting its replacement.
                let detail = cause.source_disposition_detail()?;
                let replacement = disposition(
                    target.repository,
                    target.generation,
                    input,
                    language,
                    &detail,
                    cancellation,
                )?;
                let prepared = prepare_normalized_document_append(
                    target,
                    replacement,
                    limits,
                    state,
                    cancellation,
                    Some(&mut *admission),
                    Some(&next_remaining),
                )?;
                self.unavailable = true;
                (prepared, true)
            }
            Err(error) => return Err(error),
        };
        check_cancellation(cancellation)?;
        self.remaining = next_remaining;
        self.next = self.next.checked_add(1).ok_or(FirstSliceError::Limits)?;
        Ok((prepared, replaced))
    }

    pub(super) fn finish(&self) -> Result<(), FirstSliceError> {
        (self.next == self.entries.len())
            .then_some(())
            .ok_or(FirstSliceError::Identity)
    }

    pub(super) const fn unavailable(&self) -> bool {
        self.unavailable
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a native disposition consumes its exact reserved source slot"
    )]
    pub(super) fn consume_existing_disposition(
        &mut self,
        target: &NormalizedIrDocument,
        state: &DocumentAppendState,
        input: &RustSourceInput,
        language: &str,
        source: NormalizedIrDocument,
        limits: &IrLimits,
        cancellation: &Cancellation,
        admission: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<PreparedDocumentAppend, FirstSliceError> {
        let (prepared, replaced) = self.choose(
            target,
            state,
            input,
            language,
            source,
            limits,
            cancellation,
            admission,
        )?;
        if replaced {
            return Err(FirstSliceError::Identity);
        }
        self.unavailable = true;
        Ok(prepared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileIdentity, derive_file};

    fn input(repository: RepositoryId) -> RustSourceInput {
        let path = RelativePath::parse(Path::new("source.rs")).unwrap();
        let file = derive_file(FileIdentity {
            repository,
            path_identity: path.identity_bytes(),
        })
        .id();
        let bytes = b"pub fn source() {}\n".to_vec();
        RustSourceInput {
            snapshot: SourceSnapshot::from_persisted(
                repository,
                path,
                file,
                content_hash(&bytes),
                bytes,
            )
            .unwrap(),
            encoding: "utf-8".to_owned(),
            generated: false,
            origins: Vec::new(),
        }
    }

    #[test]
    fn foreign_or_corrupt_fragment_binding_never_advances_reserved_sources() {
        let repository = derive_repository(b"aggregate-bound-input").id();
        let generation = GenerationId::from_bytes([93; 20]);
        let sources = [input(repository)];
        let languages = BTreeMap::from([(sources[0].snapshot.file(), "rust".to_owned())]);
        let service = FirstSliceService::new(2).unwrap();
        let mut floor =
            ensure_generation_memory_preflight(sources[0].snapshot.metadata().length).unwrap();
        let mut memory = service
            .reserve_generation_memory(generation, floor, PendingGenerationMemory::Reserved)
            .unwrap();
        let mut reserved = Reservation::new(
            repository,
            generation,
            &sources,
            &languages,
            &BTreeMap::new(),
            service.analysis_limits.ir(),
            &mut floor,
            &mut memory,
            false,
            &Cancellation::new(),
        )
        .unwrap()
        .unwrap();
        let target = NormalizedIrDocument::empty(repository, generation);
        let state = DocumentAppendState::from_document(&target).unwrap();
        let valid = disposition(
            repository,
            generation,
            &sources[0],
            "rust",
            "bound source",
            &Cancellation::new(),
        )
        .unwrap();
        for foreign in [true, false] {
            let mut source = valid.clone();
            if foreign {
                source.generation = GenerationId::from_bytes([94; 20]);
            } else {
                source.files[0].content_hash = content_hash(b"corrupt source identity");
            }
            assert!(matches!(
                reserved.choose(
                    &target,
                    &state,
                    &sources[0],
                    "rust",
                    source,
                    service.analysis_limits.ir(),
                    &Cancellation::new(),
                    &mut |_| Ok(())
                ),
                Err(FirstSliceError::Identity)
            ));
            assert!(reserved.finish().is_err());
            assert!(!reserved.unavailable());
            assert!(target.files.is_empty());
        }
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            reserved.choose(
                &target,
                &state,
                &sources[0],
                "rust",
                valid.clone(),
                service.analysis_limits.ir(),
                &cancelled,
                &mut |_| Ok(())
            ),
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ));
        assert!(reserved.finish().is_err());
        assert!(!reserved.unavailable());
        assert!(
            !reserved
                .choose(
                    &target,
                    &state,
                    &sources[0],
                    "rust",
                    valid,
                    service.analysis_limits.ir(),
                    &Cancellation::new(),
                    &mut |_| Ok(())
                )
                .unwrap()
                .1
        );
        reserved.finish().unwrap();
        drop(reserved);
        drop(memory);
        assert_eq!(
            service
                .generation_cache
                .lock()
                .unwrap()
                .reserved_memory_bytes()
                .unwrap(),
            0
        );
    }

    #[test]
    fn cancelled_or_unavailable_minimum_releases_optional_table_before_original_pipeline() {
        let repository = derive_repository(b"aggregate-reservation-refusal").id();
        let generation = GenerationId::from_bytes([95; 20]);
        let sources = [input(repository)];
        let languages = BTreeMap::from([(sources[0].snapshot.file(), "rust".to_owned())]);
        let service = FirstSliceService::new(2).unwrap();
        let original =
            ensure_generation_memory_preflight(sources[0].snapshot.metadata().length).unwrap();
        let mut floor = original;
        let mut memory = service
            .reserve_generation_memory(generation, floor, PendingGenerationMemory::Reserved)
            .unwrap();
        let mut limits = service.analysis_limits.ir().clone();
        limits.max_skipped_regions = 2;
        assert!(
            Reservation::new(
                repository,
                generation,
                &sources,
                &languages,
                &BTreeMap::new(),
                &limits,
                &mut floor,
                &mut memory,
                false,
                &Cancellation::new()
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(floor, original);
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            Reservation::new(
                repository,
                generation,
                &sources,
                &languages,
                &BTreeMap::new(),
                service.analysis_limits.ir(),
                &mut floor,
                &mut memory,
                false,
                &cancelled
            ),
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ));
        drop(memory);
        assert_eq!(
            service
                .generation_cache
                .lock()
                .unwrap()
                .reserved_memory_bytes()
                .unwrap(),
            0
        );
    }
    #[test]
    fn exact_disposition_byte_model_matches_a_genuine_combined_string_cause() {
        let repository = derive_repository(b"exact-aggregate-byte-reservation").id();
        let generation = GenerationId::from_bytes([96; 20]);
        let input = input(repository);
        let mut limits = IrLimits::default();
        let model = disposition(
            repository,
            generation,
            &input,
            "rust",
            &"x".repeat(EXPLANATION_BYTES),
            &Cancellation::new(),
        )
        .unwrap();
        let usage = IrDocumentUsage::measure(&model, &limits).unwrap();
        let other_path = RelativePath::parse(Path::new("other.rs")).unwrap();
        let other_file = derive_file(FileIdentity {
            repository,
            path_identity: other_path.identity_bytes(),
        })
        .id();
        let other_bytes = input.snapshot.content().to_vec();
        let other = RustSourceInput {
            snapshot: SourceSnapshot::from_persisted(
                repository,
                other_path,
                other_file,
                content_hash(&other_bytes),
                other_bytes,
            )
            .unwrap(),
            encoding: "utf-8".to_owned(),
            generated: false,
            origins: Vec::new(),
        };
        let other_document = disposition(
            repository,
            generation,
            &other,
            "rust",
            &"x".repeat(EXPLANATION_BYTES),
            &Cancellation::new(),
        )
        .unwrap();
        let other_usage = IrDocumentUsage::measure(&other_document, &limits).unwrap();
        limits.max_total_string_bytes =
            usage.total_string_bytes() + other_usage.total_string_bytes() - 1;
        let error = usage.clone().checked_add(other_usage, &limits).unwrap_err();
        let cause = AggregateCapacityFailure::from_combined_usage(&error).unwrap();
        assert!(cause.is_structural_append());
        let detail = cause.source_disposition_detail().unwrap();
        assert_eq!(detail.len(), EXPLANATION_BYTES);
        assert!(detail.contains("resource=StringBytes"));
        let actual = disposition(
            repository,
            generation,
            &input,
            "rust",
            &detail,
            &Cancellation::new(),
        )
        .unwrap();
        rootlight_ir::validate_ir_document(&actual, &limits, &ExtensionSupport::default()).unwrap();
        assert_eq!(IrDocumentUsage::measure(&actual, &limits).unwrap(), usage);
    }

    #[test]
    fn generated_disposition_keeps_source_identity_and_never_forgives_origin_mappings() {
        let repository = derive_repository(b"generated-physical-disposition").id();
        let generation = GenerationId::from_bytes([97; 20]);
        let mut source = input(repository);
        source.generated = true;
        let document = disposition(
            repository,
            generation,
            &source,
            "rust",
            "bound generated source",
            &Cancellation::new(),
        )
        .unwrap();
        rootlight_ir::validate_ir_document(
            &document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        let file = &document.files[0];
        assert!(file.generated);
        assert_eq!(file.id, source.snapshot.file());
        assert_eq!(file.content_hash, source.snapshot.content_hash());
        assert_eq!(file.byte_length, source.snapshot.metadata().length);
        assert_eq!(file.path, source.snapshot.path().as_str());
        assert!(document.entities.is_empty());
        assert!(document.occurrences.is_empty());
        assert!(document.relations.is_empty());
        assert!(document.source_mappings.is_empty());
        assert_eq!(document.skipped_regions.len(), 3);
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            disposition(
                repository,
                generation,
                &source,
                "rust",
                "cancelled",
                &cancelled
            ),
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ));

        let origin_path = RelativePath::parse(Path::new("origin.rs")).unwrap();
        let origin = derive_file(FileIdentity {
            repository,
            path_identity: origin_path.identity_bytes(),
        })
        .id();
        source.origins.push(GeneratedOriginMapping::new(
            SourceSpan::new(source.snapshot.file(), 0, 1).unwrap(),
            origin_path,
            SourceSpan::new(origin, 0, 1).unwrap(),
            TransformationId::new("authored-origin-control").unwrap(),
            None,
        ));
        assert!(matches!(
            disposition(
                repository,
                generation,
                &source,
                "rust",
                "origin stays strict",
                &Cancellation::new()
            ),
            Err(FirstSliceError::Identity)
        ));
    }
}
