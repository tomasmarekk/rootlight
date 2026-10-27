//! Keep accepted ordinary structural facts inside the complete durable restore budget.
//! A catalog transaction follows its owned append plan. Only an intrinsic finite
//! restore refusal selects an exact source disposition; host failures stay strict.

use rootlight_catalog::{
    OracleLayoutCensus, OracleLayoutFragment, OracleLayoutPreview, OracleReadAdmissionError,
    OracleReadMemoryPlan,
};

use super::*;

const EXPLANATION_BYTES: usize = 109;

#[derive(Debug, Serialize)]
pub(super) struct RefusalObservation {
    scope: &'static str,
    sidecar_input_scope: &'static str,
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    memory: durable::OracleRestoreMemoryBreakdown,
    reserved_sidecars: durable::restore_envelope::ReservedSidecarInputs,
    limit: u64,
}

pub(super) struct FileRestoreCapacity {
    file: FileId,
    observed: u64,
    limit: u64,
}

impl FileRestoreCapacity {
    fn from_observed(file: FileId, observed: u64) -> Option<Self> {
        let limit = durable::oracle_restore_memory_limit();
        (observed > limit).then_some(Self {
            file,
            observed,
            limit,
        })
    }

    pub(super) fn detail(&self, file: FileId) -> Result<String, FirstSliceError> {
        if file != self.file || self.observed <= self.limit {
            return Err(FirstSliceError::Identity);
        }
        let value = format!(
            "restore memory bytes observed {} above {}",
            self.observed, self.limit
        );
        if value.len() > EXPLANATION_BYTES {
            return Err(FirstSliceError::Identity);
        }
        Ok(value)
    }
}

/// Catalog callbacks report simultaneous workspace, whereas the existing host
/// ledger accepts additive allowances. Retaining the high water charges repeated
/// measurements once while preserving the original host refusal contract.
#[derive(Default)]
struct Workspace {
    peak: u64,
}

impl Workspace {
    fn admit(
        &mut self,
        bytes: u64,
        admission: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<(), FirstSliceError> {
        if bytes > self.peak {
            admission(bytes - self.peak)?;
            self.peak = bytes;
        }
        Ok(())
    }
}

fn catalog(
    error: OracleReadAdmissionError<FirstSliceError>,
    cancellation: &Cancellation,
) -> FirstSliceError {
    match error {
        OracleReadAdmissionError::Catalog(error) => map_catalog_error(&error, cancellation),
        OracleReadAdmissionError::Admission(error) => error,
    }
}

pub(super) fn scope_supported(claims: &[FileIdentityClaim], sources: &[RustSourceInput]) -> bool {
    !claims.is_empty()
        && sources.iter().all(|source| source.origins.is_empty())
        && !claims.iter().any(|claim| {
            claim.path.rsplit_once('.').is_some_and(|(_, extension)| {
                extension.eq_ignore_ascii_case("fsproj") || extension.eq_ignore_ascii_case("vbp")
            })
        })
        && !predecessor_policy()
}

fn predecessor_policy() -> bool {
    #[cfg(test)]
    {
        oracle_restore_policy_tests::PREDECESSOR_POLICY.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        false
    }
}

fn warning(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    file: FileId,
    code: &str,
    message: String,
) -> Result<DiagnosticRecord, FirstSliceError> {
    let mut diagnostic = DiagnosticRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        code: fallible_copy_string(code)?,
        message,
        severity: DiagnosticSeverity::Warning,
        source: None,
        coverage_effect: CoverageStatus::Bounded,
        provenance,
        evidence: FactEvidence {
            source: None,
            derivation: vec![FactRef::File(file)],
        },
    };
    diagnostic.id =
        derive_diagnostic_record_id(&diagnostic).map_err(|_| FirstSliceError::Identity)?;
    Ok(diagnostic)
}

/// Private maximum-width warnings reserve writer rows and opaque bytes, without
/// publishing a fabricated refusal. Actual warnings replace this floor later.
pub(super) fn host_floor(
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    oversized: u64,
) -> Result<NormalizedIrDocument, FirstSliceError> {
    let mut document = NormalizedIrDocument::empty(repository, generation);
    document.diagnostics.try_reserve_exact(2).map_err(|_| {
        FirstSliceError::ResourceUnavailable {
            resource: FirstSliceResource::MemoryBytes,
        }
    })?;
    document.diagnostics.push(warning(repository, generation, FactId::from_bytes([0; 20]), file,
        "resolution-restore-capacity", format!("complete resolution restore requires {} modeled bytes above limit {}; structural facts remain available", u64::MAX, u64::MAX))?);
    if oversized > 0 {
        document.diagnostics.push(warning(repository, generation, FactId::from_bytes([0; 20]), file,
            "oversized-inputs-bounded", format!("oversized repository input count {oversized} omitted by configured source file limit"))?);
    }
    Ok(document)
}

pub(super) struct Reservation {
    census: OracleLayoutCensus,
    floors: Vec<Slot>,
    floor_bytes: u64,
    table_bytes: u64,
    next: usize,
    workspace: Workspace,
    host_floor: OracleLayoutFragment,
    envelope: durable::restore_envelope::Envelope,
}

struct Slot {
    file: FileId,
    floor: Option<OracleLayoutFragment>,
}

impl Reservation {
    #[expect(
        clippy::too_many_arguments,
        reason = "the ordinary owner pins all physical metadata and current admission"
    )]
    pub(super) fn new(
        metadata: GenerationMetadata,
        inputs: &[ManifestInput],
        claims: &[FileIdentityClaim],
        sources: &[RustSourceInput],
        languages: &BTreeMap<FileId, String>,
        capacity: &mut aggregate_file_preservation::Reservation,
        existing: &NormalizedIrDocument,
        host: &NormalizedIrDocument,
        context: &GenerationContext<'_>,
        cancellation: &Cancellation,
        admission: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<Option<Self>, FirstSliceError> {
        check_cancellation(cancellation)?;
        if !scope_supported(claims, sources)
            || inputs.len() != claims.len()
            || !capacity.oracle_ready()
        {
            return Err(FirstSliceError::Identity);
        }
        let mut workspace = Workspace {
            peak: capacity.oracle_workspace(),
        };
        let mut floor_bytes = capacity.oracle_bytes();
        let description_bytes = claims
            .len()
            .checked_mul(size_of::<durable::restore_envelope::Source<'_>>())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(FirstSliceError::Limits)?;
        workspace.admit(
            floor_bytes
                .checked_add(description_bytes)
                .ok_or(FirstSliceError::Limits)?,
            admission,
        )?;
        let mut descriptions = Vec::new();
        descriptions.try_reserve_exact(claims.len()).map_err(|_| {
            FirstSliceError::ResourceUnavailable {
                resource: FirstSliceResource::MemoryBytes,
            }
        })?;
        let actual_descriptions = descriptions
            .capacity()
            .checked_mul(size_of::<durable::restore_envelope::Source<'_>>())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(FirstSliceError::Limits)?;
        workspace.admit(
            floor_bytes
                .checked_add(actual_descriptions)
                .ok_or(FirstSliceError::Limits)?,
            admission,
        )?;
        for (input, claim) in inputs.iter().zip(claims) {
            check_cancellation(cancellation)?;
            if input.file != claim.file
                || input.content_hash != claim.content_hash
                || input.bytes != claim.byte_length
            {
                return Err(FirstSliceError::Identity);
            }
            descriptions.push(durable::restore_envelope::Source {
                claim,
                language: languages.get(&claim.file).map_or_else(
                    || detected_source_language(input).unwrap_or("unknown"),
                    String::as_str,
                ),
                encoding: &input.encoding,
                generated: matches!(input.class, InputClass::Generated),
            });
        }
        let mut envelope_workspace = 0_u64;
        let envelope = durable::restore_envelope::Envelope::prepare(
            metadata,
            &descriptions,
            cancellation,
            &mut |bytes| {
                envelope_workspace = envelope_workspace
                    .checked_add(bytes)
                    .ok_or(FirstSliceError::Limits)?;
                workspace.admit(
                    actual_descriptions
                        .checked_add(floor_bytes)
                        .and_then(|bytes| bytes.checked_add(envelope_workspace))
                        .ok_or(FirstSliceError::Limits)?,
                    admission,
                )
            },
        )?;
        drop(descriptions);
        let table_bytes = sources
            .len()
            .checked_mul(size_of::<Slot>())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(FirstSliceError::Limits)?;
        workspace.admit(
            floor_bytes
                .checked_add(table_bytes)
                .ok_or(FirstSliceError::Limits)?,
            admission,
        )?;
        let mut floors = Vec::new();
        floors.try_reserve_exact(sources.len()).map_err(|_| {
            FirstSliceError::ResourceUnavailable {
                resource: FirstSliceResource::MemoryBytes,
            }
        })?;
        let table_bytes = floors
            .capacity()
            .checked_mul(size_of::<Slot>())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(FirstSliceError::Limits)?;
        workspace.admit(
            floor_bytes
                .checked_add(table_bytes)
                .ok_or(FirstSliceError::Limits)?,
            admission,
        )?;
        let mut census = OracleLayoutCensus::new(metadata.repository(), metadata.generation());
        for (index, input) in sources.iter().enumerate() {
            check_cancellation(cancellation)?;
            let floor = capacity.take_oracle_floor(index, input.snapshot.file())?;
            let own = floor
                .retained_bytes()
                .map_err(|error| map_catalog_error(&error, cancellation))?;
            let other = table_bytes
                .checked_add(floor_bytes)
                .and_then(|bytes| bytes.checked_add(census.retained_bytes().ok()?))
                .and_then(|bytes| bytes.checked_sub(own))
                .ok_or(FirstSliceError::Limits)?;
            let added = floor
                .duplicate(other, context, &mut |bytes| {
                    workspace.admit(bytes, admission)
                })
                .map_err(|error| catalog(error, cancellation))?;
            census
                .preview(
                    OracleLayoutFragment::empty(metadata.repository(), metadata.generation()),
                    added,
                    table_bytes
                        .checked_add(floor_bytes)
                        .ok_or(FirstSliceError::Limits)?,
                    context,
                    &mut |bytes| workspace.admit(bytes, admission),
                )
                .map_err(|error| catalog(error, cancellation))?
                .commit();
            floors.push(Slot {
                file: input.snapshot.file(),
                floor: Some(floor),
            });
        }
        let other = table_bytes
            .checked_add(floor_bytes)
            .and_then(|value| value.checked_add(census.retained_bytes().ok()?))
            .ok_or(FirstSliceError::Limits)?;
        let host_floor = OracleLayoutFragment::measure(host, other, context, &mut |bytes| {
            workspace.admit(bytes, admission)
        })
        .map_err(|error| catalog(error, cancellation))?;
        let added = host_floor
            .duplicate(other, context, &mut |bytes| {
                workspace.admit(bytes, admission)
            })
            .map_err(|error| catalog(error, cancellation))?;
        floor_bytes = floor_bytes
            .checked_add(
                host_floor
                    .retained_bytes()
                    .map_err(|error| map_catalog_error(&error, cancellation))?,
            )
            .ok_or(FirstSliceError::Limits)?;
        census
            .preview(
                OracleLayoutFragment::empty(metadata.repository(), metadata.generation()),
                added,
                table_bytes
                    .checked_add(floor_bytes)
                    .ok_or(FirstSliceError::Limits)?,
                context,
                &mut |bytes| workspace.admit(bytes, admission),
            )
            .map_err(|error| catalog(error, cancellation))?
            .commit();
        let existing = OracleLayoutFragment::measure(
            existing,
            table_bytes
                .checked_add(floor_bytes)
                .and_then(|value| value.checked_add(census.retained_bytes().ok()?))
                .ok_or(FirstSliceError::Limits)?,
            context,
            &mut |bytes| workspace.admit(bytes, admission),
        )
        .map_err(|error| catalog(error, cancellation))?;
        census
            .preview(
                OracleLayoutFragment::empty(metadata.repository(), metadata.generation()),
                existing,
                table_bytes
                    .checked_add(floor_bytes)
                    .ok_or(FirstSliceError::Limits)?,
                context,
                &mut |bytes| workspace.admit(bytes, admission),
            )
            .map_err(|error| catalog(error, cancellation))?
            .commit();
        let value = Self {
            census,
            floors,
            floor_bytes,
            table_bytes,
            next: 0,
            workspace,
            host_floor,
            envelope,
        };
        // A floor that cannot fit does not authorize dropping any physical input.
        // Its intrinsic failure remains the unchanged final strict pipeline.
        let observed = value.envelope.observed(
            value
                .census
                .read_memory_plan(context)
                .map_err(|error| map_catalog_error(&error, cancellation))?,
        )?;
        Ok((observed <= durable::oracle_restore_memory_limit()).then_some(value))
    }

    fn resident(&self) -> Result<u64, FirstSliceError> {
        self.table_bytes
            .checked_add(self.floor_bytes)
            .and_then(|bytes| bytes.checked_add(self.census.retained_bytes().ok()?))
            .ok_or(FirstSliceError::Limits)
    }

    fn delta(
        &mut self,
        target: &NormalizedIrDocument,
        prepared: &PreparedDocumentAppend,
        file: FileId,
        context: &GenerationContext<'_>,
        cancellation: &Cancellation,
        admission: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<(OracleLayoutFragment, OracleLayoutFragment), FirstSliceError> {
        check_cancellation(cancellation)?;
        let slot = self
            .floors
            .get(self.next)
            .ok_or(FirstSliceError::Identity)?;
        if slot.file != file
            || prepared.source.repository != target.repository
            || prepared.source.generation != target.generation
            || prepared.source.files.len() != 1
            || prepared.source.files[0].id != file
            || !prepared.source.source_mappings.is_empty()
        {
            return Err(FirstSliceError::Identity);
        }
        let floor = slot.floor.as_ref().ok_or(FirstSliceError::Identity)?;
        let base = self.resident()?;
        let original = floor
            .retained_bytes()
            .map_err(|error| map_catalog_error(&error, cancellation))?;
        let mut removed = floor
            .duplicate(
                base.checked_sub(original).ok_or(FirstSliceError::Limits)?,
                context,
                &mut |bytes| self.workspace.admit(bytes, admission),
            )
            .map_err(|error| catalog(error, cancellation))?;
        if let Some(index) = prepared.dropped_target_diagnostic {
            let record = target
                .diagnostics
                .get(index)
                .ok_or(FirstSliceError::Identity)?;
            let part = OracleLayoutFragment::measure_diagnostic(
                record,
                base.checked_add(
                    removed
                        .retained_bytes()
                        .map_err(|error| map_catalog_error(&error, cancellation))?,
                )
                .ok_or(FirstSliceError::Limits)?,
                context,
                &mut |bytes| self.workspace.admit(bytes, admission),
            )
            .map_err(|error| catalog(error, cancellation))?;
            removed = removed
                .merge(part, base, context, &mut |bytes| {
                    self.workspace.admit(bytes, admission)
                })
                .map_err(|error| catalog(error, cancellation))?;
        }
        for &index in &prepared.extension_plan.dropped_target_indices {
            let record = target
                .extensions
                .get(index)
                .ok_or(FirstSliceError::Identity)?;
            let part = OracleLayoutFragment::measure_extension(
                record,
                base.checked_add(
                    removed
                        .retained_bytes()
                        .map_err(|error| map_catalog_error(&error, cancellation))?,
                )
                .ok_or(FirstSliceError::Limits)?,
                context,
                &mut |bytes| self.workspace.admit(bytes, admission),
            )
            .map_err(|error| catalog(error, cancellation))?;
            removed = removed
                .merge(part, base, context, &mut |bytes| {
                    self.workspace.admit(bytes, admission)
                })
                .map_err(|error| catalog(error, cancellation))?;
        }
        let mut added = OracleLayoutFragment::measure(
            &prepared.source,
            base.checked_add(
                removed
                    .retained_bytes()
                    .map_err(|error| map_catalog_error(&error, cancellation))?,
            )
            .ok_or(FirstSliceError::Limits)?,
            context,
            &mut |bytes| self.workspace.admit(bytes, admission),
        )
        .map_err(|error| catalog(error, cancellation))?;
        for patch in &prepared.extension_plan.coverage.patches {
            check_cancellation(cancellation)?;
            let record = target
                .coverage_records
                .get(patch.index)
                .ok_or(FirstSliceError::Identity)?;
            let other = base
                .checked_add(
                    removed
                        .retained_bytes()
                        .map_err(|error| map_catalog_error(&error, cancellation))?,
                )
                .and_then(|bytes| bytes.checked_add(added.retained_bytes().ok()?))
                .ok_or(FirstSliceError::Limits)?;
            let old =
                OracleLayoutFragment::measure_coverage(record, other, context, &mut |bytes| {
                    self.workspace.admit(bytes, admission)
                })
                .map_err(|error| catalog(error, cancellation))?;
            let other_added = base
                .checked_add(
                    added
                        .retained_bytes()
                        .map_err(|error| map_catalog_error(&error, cancellation))?,
                )
                .ok_or(FirstSliceError::Limits)?;
            removed = removed
                .merge(old, other_added, context, &mut |bytes| {
                    self.workspace.admit(bytes, admission)
                })
                .map_err(|error| catalog(error, cancellation))?;
            let evidence_bytes = record
                .evidence
                .derivation
                .len()
                .checked_mul(size_of::<FactRef>())
                .and_then(|bytes| u64::try_from(bytes).ok())
                .ok_or(FirstSliceError::Limits)?;
            let other = base
                .checked_add(
                    removed
                        .retained_bytes()
                        .map_err(|error| map_catalog_error(&error, cancellation))?,
                )
                .and_then(|bytes| bytes.checked_add(added.retained_bytes().ok()?))
                .ok_or(FirstSliceError::Limits)?;
            self.workspace.admit(
                other
                    .checked_add(evidence_bytes)
                    .ok_or(FirstSliceError::Limits)?,
                admission,
            )?;
            let mut derivation = Vec::new();
            derivation
                .try_reserve_exact(record.evidence.derivation.len())
                .map_err(|_| FirstSliceError::ResourceUnavailable {
                    resource: FirstSliceResource::MemoryBytes,
                })?;
            let actual = derivation
                .capacity()
                .checked_mul(size_of::<FactRef>())
                .and_then(|bytes| u64::try_from(bytes).ok())
                .ok_or(FirstSliceError::Limits)?;
            self.workspace.admit(
                other.checked_add(actual).ok_or(FirstSliceError::Limits)?,
                admission,
            )?;
            derivation.extend_from_slice(&record.evidence.derivation);
            let adjusted = CoverageRecord {
                id: patch.id,
                repository: record.repository,
                generation: record.generation,
                scope: record.scope,
                domain: record.domain,
                tier: record.tier,
                status: patch.status,
                discovered: record.discovered,
                indexed: patch.indexed,
                skipped: patch.skipped,
                provenance: record.provenance,
                evidence: FactEvidence {
                    source: record.evidence.source.clone(),
                    derivation,
                },
            };
            let part = OracleLayoutFragment::measure_coverage(
                &adjusted,
                other.checked_add(actual).ok_or(FirstSliceError::Limits)?,
                context,
                &mut |bytes| self.workspace.admit(bytes, admission),
            )
            .map_err(|error| catalog(error, cancellation))?;
            let held_removed = base
                .checked_add(
                    removed
                        .retained_bytes()
                        .map_err(|error| map_catalog_error(&error, cancellation))?,
                )
                .and_then(|bytes| bytes.checked_add(actual))
                .ok_or(FirstSliceError::Limits)?;
            added = added
                .merge(part, held_removed, context, &mut |bytes| {
                    self.workspace.admit(bytes, admission)
                })
                .map_err(|error| catalog(error, cancellation))?;
        }
        Ok((removed, added))
    }

    pub(super) fn refusal(
        &mut self,
        target: &NormalizedIrDocument,
        prepared: &PreparedDocumentAppend,
        file: FileId,
        context: &GenerationContext<'_>,
        cancellation: &Cancellation,
        admission: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<Option<FileRestoreCapacity>, FirstSliceError> {
        let (removed, added) =
            self.delta(target, prepared, file, context, cancellation, admission)?;
        let other = self
            .table_bytes
            .checked_add(self.floor_bytes)
            .ok_or(FirstSliceError::Limits)?;
        let preview = self
            .census
            .preview(removed, added, other, context, &mut |bytes| {
                self.workspace.admit(bytes, admission)
            })
            .map_err(|error| catalog(error, cancellation))?;
        let memory = self.envelope.breakdown(
            preview
                .read_memory_plan(context)
                .map_err(|error| map_catalog_error(&error, cancellation))?,
        )?;
        let observed = memory.total_bytes;
        drop(preview);
        #[cfg(test)]
        tests::observe(file, observed);
        let refusal = FileRestoreCapacity::from_observed(file, observed);
        if let Some(refusal) = &refusal {
            with_analysis_profile(|profile| {
                if profile.first_oracle_restore_refusal.is_none() {
                    profile.first_oracle_restore_refusal = Some(RefusalObservation {
                        scope: "prospective_cumulative_restore_model_not_heap_or_rss",
                        sidecar_input_scope: "reserved_envelope_not_actual_written_descriptors",
                        repository: target.repository,
                        generation: target.generation,
                        file,
                        memory,
                        reserved_sidecars: self.envelope.reserved_sidecar_inputs(),
                        limit: refusal.limit,
                    });
                } else {
                    increment_profile_count(
                        &mut profile.later_oracle_restore_refusals,
                        &mut profile.counts_complete,
                    );
                }
            });
        }
        Ok(refusal)
    }

    pub(super) fn preview(
        &mut self,
        target: &NormalizedIrDocument,
        prepared: &PreparedDocumentAppend,
        file: FileId,
        context: &GenerationContext<'_>,
        cancellation: &Cancellation,
        admission: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<OracleLayoutPreview<'_>, FirstSliceError> {
        let (removed, added) =
            self.delta(target, prepared, file, context, cancellation, admission)?;
        let other = self
            .table_bytes
            .checked_add(self.floor_bytes)
            .ok_or(FirstSliceError::Limits)?;
        let preview = self
            .census
            .preview(removed, added, other, context, &mut |bytes| {
                self.workspace.admit(bytes, admission)
            })
            .map_err(|error| catalog(error, cancellation))?;
        let observed = self.envelope.observed(
            preview
                .read_memory_plan(context)
                .map_err(|error| map_catalog_error(&error, cancellation))?,
        )?;
        if observed > durable::oracle_restore_memory_limit() {
            // Replacing a selected file must fit its already reserved minimum.
            // A mismatch is an integrity failure, never a second rescue reason.
            return Err(FirstSliceError::Identity);
        }
        Ok(preview)
    }

    pub(super) fn consume(
        &mut self,
        file: FileId,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        let slot = self
            .floors
            .get_mut(self.next)
            .ok_or(FirstSliceError::Identity)?;
        if slot.file != file {
            return Err(FirstSliceError::Identity);
        }
        let floor = slot.floor.take().ok_or(FirstSliceError::Identity)?;
        self.floor_bytes = self
            .floor_bytes
            .checked_sub(
                floor
                    .retained_bytes()
                    .map_err(|error| map_catalog_error(&error, cancellation))?,
            )
            .ok_or(FirstSliceError::Limits)?;
        self.next = self.next.checked_add(1).ok_or(FirstSliceError::Limits)?;
        Ok(())
    }

    pub(super) fn finish(self) -> Result<FinalAdmission, FirstSliceError> {
        if self.next != self.floors.len() || self.floors.iter().any(|slot| slot.floor.is_some()) {
            return Err(FirstSliceError::Identity);
        }
        // All future physical slots closed. Host warnings occupy their private
        // reserved floor, and final owned resolution is measured independently.
        let Self {
            envelope,
            workspace,
            census,
            floors,
            host_floor,
            ..
        } = self;
        drop((census, floors, host_floor));
        Ok(FinalAdmission {
            envelope,
            workspace,
            expected_final_plan: None,
        })
    }

    #[cfg(test)]
    pub(super) fn verify_current_prefix(
        &mut self,
        document: &NormalizedIrDocument,
        context: &GenerationContext<'_>,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        let removed = self
            .host_floor
            .duplicate(0, context, &mut |_| Ok::<_, FirstSliceError>(()))
            .map_err(|error| catalog(error, cancellation))?;
        let actual = OracleLayoutFragment::measure(document, 0, context, &mut |_| {
            Ok::<_, FirstSliceError>(())
        })
        .map_err(|error| catalog(error, cancellation))?;
        let preview = self
            .census
            .preview(
                removed,
                OracleLayoutFragment::empty(document.repository, document.generation),
                0,
                context,
                &mut |_| Ok::<_, FirstSliceError>(()),
            )
            .map_err(|error| catalog(error, cancellation))?;
        let predicted = preview
            .read_memory_plan(context)
            .map_err(|error| map_catalog_error(&error, cancellation))?;
        let observed = actual
            .read_memory_plan(context)
            .map_err(|error| map_catalog_error(&error, cancellation))?;
        if predicted != observed {
            return Err(FirstSliceError::Identity);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "oracle_restore_preservation/tests.rs"]
mod tests;

/// Commit the census after its matching document append has succeeded.
/// Consuming both optional states releases the census borrow before slot updates.
pub(super) fn commit_preview(preview: Option<OracleLayoutPreview<'_>>) {
    if let Some(preview) = preview {
        preview.commit();
    }
}

pub(super) struct FinalAdmission {
    envelope: durable::restore_envelope::Envelope,
    workspace: Workspace,
    expected_final_plan: Option<OracleReadMemoryPlan>,
}

impl FinalAdmission {
    #[expect(
        clippy::too_many_arguments,
        reason = "owned resolution pins its restore and host admission contexts"
    )]
    pub(super) fn resolve(
        &mut self,
        document: NormalizedIrDocument,
        limits: ResolutionLimits,
        ir_limits: &IrLimits,
        extensions: &ExtensionSupport,
        context: &GenerationContext<'_>,
        cancellation: &Cancellation,
        admission: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<NormalizedIrDocument, FirstSliceError> {
        check_cancellation(cancellation)?;
        let document = rootlight_ir::canonicalize_ir_document(document, ir_limits, extensions)
            .map_err(|error| {
                map_resolution_error(ResolutionError::InvalidDocument(error), cancellation)
            })?;
        check_cancellation(cancellation)?;
        let mut undo_bytes = 0_u64;
        let undo = resolver_restore_undo::Undo::capture(&document, cancellation, &mut |bytes| {
            undo_bytes = undo_bytes
                .checked_add(bytes)
                .ok_or(FirstSliceError::Limits)?;
            self.workspace.admit(undo_bytes, &mut |added| {
                #[cfg(test)]
                tests::admit_undo_additional(added)?;
                admission(added)
            })
        })?;
        #[cfg(test)]
        tests::observe_captured_undo(undo_bytes);
        // Ordinary admission excludes classic and native project owners.
        let candidate = apply_bounded_resolution_with_native_projects(
            document,
            limits,
            ResolverFactContext::new(content_hash(RESOLVER_BINARY_SEED)),
            &[],
            &[],
            cancellation,
        )?;
        rootlight_ir::validate_ir_document(&candidate, ir_limits, extensions).map_err(|error| {
            map_resolution_error(ResolutionError::InvalidDocument(error), cancellation)
        })?;
        check_cancellation(cancellation)?;
        let measured =
            OracleLayoutFragment::measure(&candidate, undo_bytes, context, &mut |bytes| {
                self.workspace.admit(bytes, admission)
            })
            .map_err(|error| catalog(error, cancellation))?;
        let observed = self.envelope.observed(
            measured
                .read_memory_plan(context)
                .map_err(|error| map_catalog_error(&error, cancellation))?,
        )?;
        drop(measured);
        if observed <= durable::oracle_restore_memory_limit() {
            drop(undo);
            return Ok(candidate);
        }
        let mut document = undo.restore(candidate, cancellation)?;
        let provenance = document
            .provenance
            .first()
            .ok_or(FirstSliceError::Identity)?
            .id;
        let file = document.files.first().ok_or(FirstSliceError::Identity)?.id;
        let diagnostic = warning(
            document.repository,
            document.generation,
            provenance,
            file,
            "resolution-restore-capacity",
            format!(
                "complete resolution restore requires {observed} modeled bytes above limit {}; structural facts remain available",
                durable::oracle_restore_memory_limit()
            ),
        )?;
        for coverage in &mut document.coverage_records {
            if matches!(
                coverage.domain,
                IrFactDomain::Occurrences | IrFactDomain::Relations
            ) && coverage.status != CoverageStatus::Unknown
            {
                coverage.status = CoverageStatus::Bounded;
                coverage.id =
                    derive_coverage_record_id(coverage).map_err(|_| FirstSliceError::Identity)?;
            }
        }
        retain_priority_aggregate_diagnostic(&mut document, diagnostic, ir_limits)?;
        rootlight_ir::validate_ir_document(&document, ir_limits, extensions).map_err(|error| {
            map_resolution_error(ResolutionError::InvalidDocument(error), cancellation)
        })?;
        check_cancellation(cancellation)?;
        // The floor promised this exact outcome. Recheck with actual records;
        // divergence remains strict and does not erase the accepted graph.
        let measured = OracleLayoutFragment::measure(&document, 0, context, &mut |bytes| {
            self.workspace.admit(bytes, admission)
        })
        .map_err(|error| catalog(error, cancellation))?;
        let observed = self.envelope.observed(
            measured
                .read_memory_plan(context)
                .map_err(|error| map_catalog_error(&error, cancellation))?,
        )?;
        if observed > durable::oracle_restore_memory_limit() {
            return Err(FirstSliceError::Identity);
        }
        Ok(document)
    }

    pub(super) fn verify_written(
        &self,
        prepared: &durable::DurablePreparedGeneration,
        plan: OracleReadMemoryPlan,
    ) -> Result<(), FirstSliceError> {
        if self.expected_final_plan != Some(plan) {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        self.envelope.verify_written(prepared)
    }

    pub(super) fn prepare_final(
        &mut self,
        document: NormalizedIrDocument,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        context: &GenerationContext<'_>,
        cancellation: &Cancellation,
        admission: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<NormalizedIrDocument, FirstSliceError> {
        check_cancellation(cancellation)?;
        let document = rootlight_ir::canonicalize_ir_document(document, limits, extensions)
            .map_err(|error| {
                map_resolution_error(ResolutionError::InvalidDocument(error), cancellation)
            })?;
        check_cancellation(cancellation)?;
        let measured = OracleLayoutFragment::measure(&document, 0, context, &mut |bytes| {
            self.workspace.admit(bytes, admission)
        })
        .map_err(|error| catalog(error, cancellation))?;
        let plan = measured
            .read_memory_plan(context)
            .map_err(|error| map_catalog_error(&error, cancellation))?;
        if self.envelope.observed(plan)? > durable::oracle_restore_memory_limit() {
            return Err(FirstSliceError::Identity);
        }
        self.expected_final_plan = Some(plan);
        Ok(document)
    }
}
