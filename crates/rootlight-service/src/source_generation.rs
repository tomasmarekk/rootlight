//! Leased source-only v5 generations with independent, bounded memory ownership.
//! Full semantic/lexical recovery remains the fallback for legacy generations.

use super::*;
use rootlight_source::{VerifiedSourceAuthority, VerifiedSourceBacking};

pub(super) struct SourceLease {
    pub(super) authority: VerifiedSourceAuthority,
    receipt: FirstSliceIndexReceipt,
    _reservation: RestoredMemoryReservation,
}

pub(super) struct SourceBacking<'a> {
    pub(super) service: &'a FirstSliceService,
    pub(super) lease: &'a SourceLease,
}

impl std::fmt::Debug for SourceBacking<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceBacking")
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

impl VerifiedSourceBacking for SourceBacking<'_> {
    fn repository(&self) -> RepositoryId {
        self.lease.authority.metadata().repository()
    }
    fn generation(&self) -> GenerationId {
        self.lease.authority.metadata().generation()
    }
    fn visit_verified(
        &self,
        file: &FileRecord,
        maximum: u64,
        cancellation: &Cancellation,
        visitor: &mut dyn FnMut(u64, &[u8]) -> Result<(), SourceError>,
    ) -> Result<u64, SourceError> {
        if self.lease.authority.find_file(file.id) != Some(file) {
            return Err(SourceError::StaleSource);
        }
        if file.byte_length > maximum {
            return Err(SourceError::VerificationBudgetExceeded);
        }
        let mut callback_error = None;
        let mut callback = |offset, bytes: &[u8]| {
            visitor(offset, bytes).map_err(|error| {
                callback_error = Some(error);
                rootlight_vfs::platform::PlatformError::SecurityPolicy
            })
        };
        let result = self
            .service
            .durable
            .as_ref()
            .ok_or(SourceError::StaleSource)?
            .visit_source(file, maximum, cancellation, &mut callback);
        if let Some(error) = callback_error {
            return Err(error);
        }
        cancellation
            .check()
            .map_err(|error| SourceError::Cancelled(error.reason()))?;
        result.map_err(|_| SourceError::StaleSource)
    }
}

impl FirstSliceService {
    pub(super) fn evict_unleased_source_authorities(&self) -> Result<(), FirstSliceError> {
        self.source_authorities
            .lock()
            .map_err(|_| FirstSliceError::Retention)?
            .retain(|_, lease| Arc::strong_count(lease) > 1);
        Ok(())
    }

    pub(super) fn source_only_lease(
        &self,
        generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<Option<Arc<SourceLease>>, FirstSliceError> {
        check_cancellation(cancellation)?;
        if self
            .generation_cache
            .lock()
            .map_err(|_| FirstSliceError::Retention)?
            .generations
            .is_loaded(generation)
        {
            return Ok(None);
        }
        let Some(durable) = &self.durable else {
            return Ok(None);
        };
        let mut cache = self
            .source_authorities
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        if let Some(lease) = cache.get(&generation) {
            // A cached metadata lease does not extend durable retention. Recheck
            // publication before serving another operation from that cache.
            if let Some((_, receipt)) = durable.source_authority_descriptor(
                lease.receipt.repository,
                generation,
                cancellation,
            )? {
                if receipt != lease.receipt {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                return Ok(Some(Arc::clone(lease)));
            }
            cache.remove(&generation);
            return Ok(None);
        }
        let Some(repository) = durable.source_authority_owner(generation, cancellation)? else {
            return Ok(None);
        };
        let (bytes, receipt) = durable
            .source_authority_descriptor(repository, generation, cancellation)?
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        // Encoded bytes plus canonical entries, recipe copies, path decoding and
        // tree workspace stay charged for the lease's whole lifetime.
        let charge = bytes
            .checked_mul(16)
            .and_then(|bytes| bytes.checked_add(GENERATION_MEMORY_FIXED_OVERHEAD_BYTES))
            .ok_or(FirstSliceError::Limits)?;
        let reserve =
            || RestoredMemoryReservation::new(Arc::clone(&self.generation_cache), charge, true);
        let reservation = match reserve() {
            Ok(value) => value,
            Err(FirstSliceError::GenerationMemoryLimit { .. }) => {
                cache.retain(|_, lease| Arc::strong_count(lease) > 1);
                reserve()?
            }
            Err(error) => return Err(error),
        };
        let authority = durable.restore_source_authority(repository, generation, cancellation)?;
        let maximum = self
            .maximum_generations_per_repository
            .checked_mul(self.maximum_repositories)
            .ok_or(FirstSliceError::Limits)?;
        if cache.len() >= maximum {
            cache.retain(|_, lease| Arc::strong_count(lease) > 1);
            if cache.len() >= maximum {
                return Err(FirstSliceError::Retention);
            }
        }
        let lease = Arc::new(SourceLease {
            authority,
            receipt,
            _reservation: reservation,
        });
        cache.insert(generation, Arc::clone(&lease));
        Ok(Some(lease))
    }

    /// Selects an authenticated source generation without advertising graph readiness.
    ///
    /// Legacy generations use the unchanged full-generation selection contract.
    /// # Errors
    /// Rejects unavailable, foreign, corrupt or resource-inadmissible source state.
    pub fn resolve_source_generation(
        &self,
        repository: RepositoryId,
        selected: Option<GenerationId>,
        cancellation: &Cancellation,
    ) -> Result<GenerationId, FirstSliceError> {
        Ok(self
            .resolve_source_context(repository, selected, cancellation)?
            .generation)
    }

    /// Resolves publication metadata without claiming that semantic recovery is ready.
    ///
    /// # Errors
    /// Preserves missing-repository and missing-generation errors, and rejects
    /// foreign, corrupt or inadmissible v5 source authority.
    pub fn resolve_source_context(
        &self,
        repository: RepositoryId,
        selected: Option<GenerationId>,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceGenerationContext, FirstSliceError> {
        check_cancellation(cancellation)?;
        let selection_error = match self.resolve_generation(repository, selected) {
            Ok(context) => return Ok(context),
            Err(error) => error,
        };
        let Some(durable) = self.durable.as_ref() else {
            return Err(selection_error);
        };
        let active = durable
            .active_restore_targets()?
            .into_iter()
            .find(|target| target.repository() == repository)
            .map(|target| target.generation())
            .ok_or(selection_error)?;
        let generation = selected.unwrap_or(active);
        let lease = self
            .source_only_lease(generation, cancellation)?
            .ok_or(FirstSliceError::GenerationNotFound)?;
        if lease.authority.metadata().repository() != repository {
            return Err(FirstSliceError::GenerationMismatch);
        }
        // A v5 parent must agree with that generation's verified recipe, even
        // when the caller selected its predecessor. Its compact lease uses the
        // same admission as selected sources; no semantic graph is loaded.
        let active_parent = if generation == active {
            lease.authority.metadata().parent()
        } else if let Ok(active_context) = self.resolve_generation(repository, Some(active)) {
            active_context.parent
        } else if let Some(active_lease) = self.source_only_lease(active, cancellation)? {
            if active_lease.authority.metadata().repository() != repository {
                return Err(FirstSliceError::GenerationMismatch);
            }
            active_lease.authority.metadata().parent()
        } else {
            durable.legacy_source_generation_parent(repository, active, cancellation)?
        };
        Ok(FirstSliceGenerationContext {
            repository,
            generation,
            parent: lease.receipt.parent,
            active_generation: active,
            active_parent,
            active: generation == active,
            receipt: lease.receipt.clone(),
        })
    }

    /// Returns authenticated publication freshness without inspecting semantic rows.
    ///
    /// # Errors
    /// Preserves source generation selection and provider identity failures.
    pub fn source_generation_freshness(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceFreshnessStatus, FirstSliceError> {
        if self
            .resolve_generation(repository, Some(generation))
            .is_ok()
        {
            return self.generation_freshness(repository, generation);
        }
        let context = self.resolve_source_context(repository, Some(generation), cancellation)?;
        let lease = self
            .source_only_lease(generation, cancellation)?
            .ok_or(FirstSliceError::GenerationNotFound)?;
        self.freshness_for_context(&context, lease.authority.metadata().provider_set_hash())
    }

    /// Gets the exact source-file language while analysis coverage remains unknown.
    ///
    /// # Errors
    /// Rejects unavailable source authority or a file outside its immutable catalog.
    pub fn source_file_language_until(
        &self,
        generation: GenerationId,
        file: FileId,
        cancellation: &Cancellation,
    ) -> Result<String, FirstSliceError> {
        if let Some(lease) = self.source_only_lease(generation, cancellation)? {
            return lease
                .authority
                .find_file(file)
                .map(|file| file.language.clone())
                .ok_or(FirstSliceError::Identity);
        }
        self.source_language_coverage_until(generation, file, cancellation)
            .map(|(language, _)| language)
    }

    /// Reports whether an exact source selection can use verified source-only state.
    ///
    /// VBA reads retain the full guarded conditional-activity path.
    /// # Errors
    /// Preserves source identity, integrity, control and admission failures.
    pub fn source_only_read_available(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
        references: &[SourceRef],
        cancellation: &Cancellation,
    ) -> Result<bool, FirstSliceError> {
        let Some(lease) = self.source_only_lease(generation, cancellation)? else {
            return Ok(false);
        };
        if lease.authority.metadata().repository() != repository {
            return Err(FirstSliceError::GenerationMismatch);
        }
        for reference in references {
            check_cancellation(cancellation)?;
            if reference.repository() != repository || reference.generation() != generation {
                return Err(FirstSliceError::GenerationMismatch);
            }
            if lease
                .authority
                .find_file(reference.span().file())
                .is_some_and(|file| {
                    matches!(file.language.as_str(), "vba" | "visual-basic-classic")
                })
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
