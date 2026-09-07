use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use rootlight_ids::{FileId, GenerationId};
use rootlight_search::{LexicalSearch, SourceLexicalCoverage};
use rootlight_storage::{GenerationMetadata, GenerationSnapshot, IdentityVerifiedGeneration};

use crate::{QueryError, QueryService};

const HARD_MAX_RETAINED_GENERATIONS: usize = 8_193;

/// Bounded registry for immutable query-generation identities and payloads.
///
/// A committed entry may retain a loaded identity-verified snapshot and search
/// reader or only immutable metadata. Durable publication, cache admission, and
/// exact reload are coordinated by the service layer; logical retention does
/// not depend on payload residency.
pub struct GenerationSet<Search> {
    maximum: usize,
    active: Option<GenerationId>,
    generations: BTreeMap<GenerationId, RetainedGeneration<Search>>,
    staged: BTreeMap<GenerationId, RetainedGeneration<Search>>,
}

/// Query-scoped ownership of one loaded immutable generation.
///
/// A live lease pins the normalized snapshot and lexical reader, so cache
/// eviction refuses to unload that entry. Callers may release the cache lock
/// before executing a bounded query.
pub struct GenerationLease<Search> {
    retained: Arc<LoadedGeneration<Search>>,
}

impl<Search> GenerationLease<Search>
where
    Search: LexicalSearch,
{
    /// Returns a typed query service over the leased immutable generation.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationMismatch`] if retained query state no
    /// longer agrees on one generation identity.
    pub fn query(&self) -> Result<QueryService<'_, Search>, QueryError> {
        QueryService::new(&self.retained.snapshot, &self.retained.search)
    }

    /// Returns the leased canonical generation snapshot.
    #[must_use]
    pub fn generation(&self) -> &GenerationSnapshot {
        &self.retained.snapshot
    }

    /// Returns generation-pinned lexical accounting without rereading source bytes.
    /// Legacy projections and backends without accounting return `None`.
    #[must_use]
    pub fn source_lexical_coverage(&self, file: FileId) -> Option<SourceLexicalCoverage> {
        self.retained.search.source_coverage(file)
    }
}

impl<Search> GenerationSet<Search>
where
    Search: LexicalSearch,
{
    /// Creates a bounded generation set.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidGenerationSet`] when the retention count
    /// is zero or exceeds the first-slice hard ceiling.
    pub fn new(maximum: usize) -> Result<Self, QueryError> {
        if maximum == 0 || maximum > HARD_MAX_RETAINED_GENERATIONS {
            return Err(QueryError::InvalidGenerationSet);
        }
        Ok(Self {
            maximum,
            active: None,
            generations: BTreeMap::new(),
            staged: BTreeMap::new(),
        })
    }

    /// Retains one identity-verified generation and matching lexical reader.
    ///
    /// When `make_active` is true, subsequent active selection names this
    /// generation while every previously retained generation remains
    /// addressable.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for generation mismatch, duplicate identity, or
    /// exhausted retention capacity.
    pub fn publish(
        &mut self,
        generation: IdentityVerifiedGeneration,
        search: Search,
        make_active: bool,
    ) -> Result<GenerationId, QueryError> {
        let snapshot = generation.into_snapshot();
        let id = snapshot.metadata().generation();
        if search.generation() != id {
            return Err(QueryError::GenerationMismatch);
        }
        if self.generations.contains_key(&id) || self.staged.contains_key(&id) {
            return Err(QueryError::DuplicateGeneration);
        }
        if self.generations.len() + self.staged.len() >= self.maximum {
            return Err(QueryError::RetentionLimit);
        }
        self.generations
            .insert(id, RetainedGeneration::loaded(snapshot, search));
        if make_active {
            self.active = Some(id);
        }
        Ok(id)
    }

    /// Reserves and retains one verified generation without making it queryable.
    ///
    /// Staging is the pre-terminal half of daemon publication. It performs all
    /// validation and retention admission before durable operation success,
    /// while [`Self::query`] and [`Self::generation`] continue to expose only
    /// committed generations.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for generation mismatch, duplicate identity, or
    /// exhausted combined committed-and-staged capacity.
    pub fn stage(
        &mut self,
        generation: IdentityVerifiedGeneration,
        search: Search,
    ) -> Result<GenerationId, QueryError> {
        let snapshot = generation.into_snapshot();
        let id = snapshot.metadata().generation();
        if search.generation() != id {
            return Err(QueryError::GenerationMismatch);
        }
        if self.generations.contains_key(&id) || self.staged.contains_key(&id) {
            return Err(QueryError::DuplicateGeneration);
        }
        if self.generations.len() + self.staged.len() >= self.maximum {
            return Err(QueryError::RetentionLimit);
        }
        self.staged
            .insert(id, RetainedGeneration::loaded(snapshot, search));
        Ok(id)
    }

    /// Commits one staged generation and optionally selects it as active.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when the staging token no
    /// longer names a reserved generation.
    pub fn commit_staged(
        &mut self,
        generation: GenerationId,
        make_active: bool,
    ) -> Result<(), QueryError> {
        let retained = self
            .staged
            .remove(&generation)
            .ok_or(QueryError::GenerationNotFound)?;
        self.generations.insert(generation, retained);
        if make_active {
            self.active = Some(generation);
        }
        Ok(())
    }

    /// Moves one just-committed generation back to hidden staging.
    ///
    /// This is the in-process compensation path when an external durable
    /// activation marker fails after retention commit.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when the committed
    /// generation is absent, or [`QueryError::InvalidGenerationSet`] when the
    /// supplied prior active generation is not retained.
    pub fn rollback_commit(
        &mut self,
        generation: GenerationId,
        prior_active: Option<GenerationId>,
    ) -> Result<(), QueryError> {
        if prior_active.is_some_and(|active| !self.generations.contains_key(&active)) {
            return Err(QueryError::InvalidGenerationSet);
        }
        if self.staged.contains_key(&generation) {
            return Err(QueryError::DuplicateGeneration);
        }
        let retained = self
            .generations
            .remove(&generation)
            .ok_or(QueryError::GenerationNotFound)?;
        self.staged.insert(generation, retained);
        if self.active == Some(generation) {
            self.active = prior_active;
        }
        Ok(())
    }

    /// Discards one staged, still-hidden generation.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when no matching reservation
    /// exists.
    pub fn discard_staged(&mut self, generation: GenerationId) -> Result<(), QueryError> {
        self.staged
            .remove(&generation)
            .map(|_| ())
            .ok_or(QueryError::GenerationNotFound)
    }

    /// Returns a typed query service for one retained immutable generation.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when the identity is no
    /// longer retained.
    pub fn query(&self, generation: GenerationId) -> Result<QueryService<'_, Search>, QueryError> {
        let retained = self
            .generations
            .get(&generation)
            .ok_or(QueryError::GenerationNotFound)?;
        let RetainedGeneration::Loaded(retained) = retained else {
            return Err(QueryError::GenerationNotFound);
        };
        QueryService::new(&retained.snapshot, &retained.search)
    }

    /// Leases one loaded immutable generation independently of this set.
    ///
    /// The returned lease pins the loaded payload until it is dropped. An
    /// unloaded durable generation remains retained but returns
    /// [`QueryError::GenerationNotFound`] until a caller reloads it.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when the identity is absent
    /// or currently unloaded.
    pub fn lease(&self, generation: GenerationId) -> Result<GenerationLease<Search>, QueryError> {
        let retained = self
            .generations
            .get(&generation)
            .ok_or(QueryError::GenerationNotFound)?;
        let RetainedGeneration::Loaded(retained) = retained else {
            return Err(QueryError::GenerationNotFound);
        };
        Ok(GenerationLease {
            retained: Arc::clone(retained),
        })
    }

    /// Returns the active immutable generation identity.
    #[must_use]
    pub const fn active_generation(&self) -> Option<GenerationId> {
        self.active
    }

    /// Selects an already-retained generation as the active query default.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when the identity is no
    /// longer retained.
    pub fn activate(&mut self, generation: GenerationId) -> Result<(), QueryError> {
        if !self.generations.contains_key(&generation) {
            return Err(QueryError::GenerationNotFound);
        }
        self.active = Some(generation);
        Ok(())
    }

    /// Removes one committed generation that is no longer selected globally.
    ///
    /// Repository-aware callers must additionally ensure the generation is
    /// not active for its owning repository before invoking this method.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when the generation is not
    /// retained, or [`QueryError::InvalidGenerationSet`] when it is the global
    /// active selection.
    pub fn remove(&mut self, generation: GenerationId) -> Result<(), QueryError> {
        if self.active == Some(generation) {
            return Err(QueryError::InvalidGenerationSet);
        }
        self.generations
            .remove(&generation)
            .map(|_| ())
            .ok_or(QueryError::GenerationNotFound)
    }

    /// Removes a checked set of committed generations and selects a replacement.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when any removed or
    /// replacement generation is absent, or [`QueryError::InvalidGenerationSet`]
    /// when the replacement is itself removed or does not preserve an
    /// unaffected active selection.
    pub fn remove_many(
        &mut self,
        removed: &BTreeSet<GenerationId>,
        replacement_active: Option<GenerationId>,
    ) -> Result<(), QueryError> {
        if removed
            .iter()
            .any(|generation| !self.generations.contains_key(generation))
            || replacement_active
                .is_some_and(|generation| !self.generations.contains_key(&generation))
        {
            return Err(QueryError::GenerationNotFound);
        }
        if replacement_active.is_some_and(|generation| removed.contains(&generation))
            || self.active.is_some_and(|active| !removed.contains(&active))
                && replacement_active != self.active
            || self.active.is_some_and(|active| removed.contains(&active))
                && replacement_active.is_none()
                && self.generations.len() != removed.len()
        {
            return Err(QueryError::InvalidGenerationSet);
        }
        for generation in removed {
            self.generations.remove(generation);
        }
        self.active = replacement_active;
        Ok(())
    }

    /// Returns whether one committed generation is retained.
    #[must_use]
    pub fn contains(&self, generation: GenerationId) -> bool {
        self.generations.contains_key(&generation)
    }

    /// Returns a retained normalized generation for source-service binding.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when the identity is not
    /// retained.
    pub fn generation(&self, generation: GenerationId) -> Result<&GenerationSnapshot, QueryError> {
        self.generations
            .get(&generation)
            .and_then(RetainedGeneration::loaded_entry)
            .map(|retained| &retained.snapshot)
            .ok_or(QueryError::GenerationNotFound)
    }

    /// Returns retained generation metadata without loading its payload.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when the generation identity
    /// is not retained.
    pub fn metadata(&self, generation: GenerationId) -> Result<GenerationMetadata, QueryError> {
        self.generations
            .get(&generation)
            .map(RetainedGeneration::metadata)
            .ok_or(QueryError::GenerationNotFound)
    }

    /// Returns whether a retained generation currently has a loaded payload.
    #[must_use]
    pub fn is_loaded(&self, generation: GenerationId) -> bool {
        self.generations
            .get(&generation)
            .is_some_and(RetainedGeneration::is_loaded)
    }

    /// Unloads one unpinned committed payload while retaining its metadata.
    ///
    /// Returns `false` when a live [`GenerationLease`] pins the payload. An
    /// already unloaded generation is an idempotent success.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when the generation identity
    /// is not retained.
    pub fn unload(&mut self, generation: GenerationId) -> Result<bool, QueryError> {
        let retained = self
            .generations
            .get_mut(&generation)
            .ok_or(QueryError::GenerationNotFound)?;
        let RetainedGeneration::Loaded(loaded) = retained else {
            return Ok(true);
        };
        if Arc::strong_count(loaded) != 1 {
            return Ok(false);
        }
        let metadata = loaded.snapshot.metadata();
        *retained = RetainedGeneration::Unloaded(metadata);
        Ok(true)
    }

    /// Reloads one metadata-only committed generation.
    ///
    /// The identity-verified snapshot and lexical reader must exactly match the
    /// retained metadata. Reloading a payload that is already present is
    /// rejected instead of replacing a possibly pinned generation.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an absent or already loaded identity,
    /// generation mismatch, or metadata mismatch.
    pub fn reload(
        &mut self,
        generation: IdentityVerifiedGeneration,
        search: Search,
    ) -> Result<GenerationId, QueryError> {
        let snapshot = generation.into_snapshot();
        let id = snapshot.metadata().generation();
        if search.generation() != id {
            return Err(QueryError::GenerationMismatch);
        }
        let retained = self
            .generations
            .get_mut(&id)
            .ok_or(QueryError::GenerationNotFound)?;
        match retained {
            RetainedGeneration::Loaded(_) => Err(QueryError::DuplicateGeneration),
            RetainedGeneration::Unloaded(metadata) => {
                if *metadata != snapshot.metadata() {
                    return Err(QueryError::GenerationMismatch);
                }
                *retained = RetainedGeneration::loaded(snapshot, search);
                Ok(id)
            }
        }
    }

    /// Returns the number of retained immutable generations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.generations.len()
    }

    /// Returns whether no immutable generation is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.generations.is_empty()
    }
}

impl<Search> std::fmt::Debug for GenerationSet<Search>
where
    Search: LexicalSearch,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GenerationSet")
            .field("maximum", &self.maximum)
            .field("active", &self.active)
            .field("retained", &self.generations.len())
            .field("staged", &self.staged.len())
            .finish()
    }
}

enum RetainedGeneration<Search> {
    Loaded(Arc<LoadedGeneration<Search>>),
    Unloaded(GenerationMetadata),
}

impl<Search> RetainedGeneration<Search> {
    fn loaded(snapshot: GenerationSnapshot, search: Search) -> Self {
        Self::Loaded(Arc::new(LoadedGeneration { snapshot, search }))
    }

    fn metadata(&self) -> GenerationMetadata {
        match self {
            Self::Loaded(retained) => retained.snapshot.metadata(),
            Self::Unloaded(metadata) => *metadata,
        }
    }

    fn loaded_entry(&self) -> Option<&LoadedGeneration<Search>> {
        match self {
            Self::Loaded(retained) => Some(retained),
            Self::Unloaded(_) => None,
        }
    }

    const fn is_loaded(&self) -> bool {
        matches!(self, Self::Loaded(_))
    }
}

struct LoadedGeneration<Search> {
    snapshot: GenerationSnapshot,
    search: Search,
}
