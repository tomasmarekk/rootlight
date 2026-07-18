use std::collections::BTreeMap;

use rootlight_ids::GenerationId;
use rootlight_search::LexicalSearch;
use rootlight_storage::{GenerationSnapshot, IdentityVerifiedGeneration};

use crate::{QueryError, QueryService};

const HARD_MAX_RETAINED_GENERATIONS: usize = 64;

/// Bounded in-memory first-slice registry for immutable query generations.
///
/// This registry proves pinned old-generation reads without claiming the
/// durable publication, lease, recovery, or reclamation semantics scheduled
/// for later milestones.
pub struct GenerationSet<Search> {
    maximum: usize,
    active: Option<GenerationId>,
    generations: BTreeMap<GenerationId, RetainedGeneration<Search>>,
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
        if self.generations.contains_key(&id) {
            return Err(QueryError::DuplicateGeneration);
        }
        if self.generations.len() >= self.maximum {
            return Err(QueryError::RetentionLimit);
        }
        self.generations
            .insert(id, RetainedGeneration { snapshot, search });
        if make_active {
            self.active = Some(id);
        }
        Ok(id)
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
        QueryService::new(&retained.snapshot, &retained.search)
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

    /// Returns a retained normalized generation for source-service binding.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::GenerationNotFound`] when the identity is not
    /// retained.
    pub fn generation(&self, generation: GenerationId) -> Result<&GenerationSnapshot, QueryError> {
        self.generations
            .get(&generation)
            .map(|retained| &retained.snapshot)
            .ok_or(QueryError::GenerationNotFound)
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
            .finish()
    }
}

struct RetainedGeneration<Search> {
    snapshot: GenerationSnapshot,
    search: Search,
}
