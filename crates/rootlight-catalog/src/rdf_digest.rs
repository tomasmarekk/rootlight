//! Digest-only RDF reads over a complete authenticated logical Oracle.
//!
//! One operation carries the same cumulative indexed-read budget across its
//! bounded ID merge and exact rereads. The adapter grants no new authority.

use super::*;
use rootlight_ir::{ExtensionEnvelope, NormalizedRebindError, RdfDigestSource};
use std::{cmp::Reverse, collections::BinaryHeap};

/// Bounded RDF digest input borrowing a fully authenticated logical Oracle.
pub struct OracleRdfDigestSource<'reader, 'request> {
    reader: &'reader OracleReader,
    context: GenerationContext<'request>,
    state: indexed_read::IndexedReadState,
    count: usize,
    failure: Option<CatalogError>,
    poisoned: bool,
}

impl<'reader, 'request> OracleRdfDigestSource<'reader, 'request> {
    /// Opens one digest operation after all committed leaves have authenticated.
    ///
    /// # Errors
    /// Rejects a canonical-only reader, cumulative count overflow, or control refusal.
    pub fn new(
        reader: &'reader OracleReader,
        context: GenerationContext<'request>,
    ) -> Result<Self, CatalogError> {
        if reader.rdf_partitions.is_empty() {
            return Err(CatalogError::new(
                CatalogErrorKind::PartitionedMaterializationRequired,
            ));
        }
        let count = reader
            .rdf_partitions
            .iter()
            .try_fold(0_u64, |total, part| {
                total
                    .checked_add(part.descriptor().record_count())
                    .ok_or_else(corrupt)
            })?;
        context
            .require(GenerationResource::Rows, count)
            .map_err(CatalogError::control)?;
        Ok(Self {
            reader,
            context,
            state: indexed_read::IndexedReadState::new(&context)?,
            count: usize::try_from(count).map_err(|_| corrupt())?,
            failure: None,
            poisoned: false,
        })
    }
    /// Returns the exact authenticated number of logical RDF statements.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Transfers the original typed backend failure after digest projection refuses.
    #[must_use]
    pub fn take_error(&mut self) -> Option<CatalogError> {
        self.failure.take()
    }

    fn reject(&mut self, error: CatalogError) -> NormalizedRebindError {
        self.poisoned = true;
        let interrupted = self.context.check().is_err();
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        if interrupted {
            NormalizedRebindError::Interrupted
        } else {
            NormalizedRebindError::IdentityRecipe
        }
    }
    fn check(&mut self) -> Result<(), NormalizedRebindError> {
        if self.poisoned {
            return Err(NormalizedRebindError::IdentityRecipe);
        }
        self.context
            .check()
            .map_err(|error| self.reject(CatalogError::control(error)))
    }
    fn head(
        &mut self,
        owner: usize,
        after: Option<FactId>,
    ) -> Result<Option<FactId>, NormalizedRebindError> {
        self.check()?;
        let part = self
            .reader
            .rdf_partitions
            .get(owner)
            .ok_or(NormalizedRebindError::IdentityRecipe)?;
        let ids = part
            .select_ids(after, 1, &self.context)
            .map_err(|error| self.reject(error))?;
        if ids.len() > 1 {
            return Err(self.reject(corrupt()));
        }
        if !ids.is_empty() {
            self.state
                .observe_row(&self.context)
                .map_err(|error| self.reject(error))?;
        }
        Ok(ids.first().copied())
    }
    fn selected(
        &mut self,
        owner: usize,
        id: FactId,
    ) -> Result<ExtensionEnvelope, NormalizedRebindError> {
        self.check()?;
        let part = self
            .reader
            .rdf_partitions
            .get(owner)
            .ok_or(NormalizedRebindError::IdentityRecipe)?;
        let mut values = part
            .read_selected_for_set(&[id], &mut self.state, &self.context)
            .map_err(|error| self.reject(error))?;
        if values.len() != 1 {
            return Err(self.reject(corrupt()));
        }
        let record = values.pop().ok_or(NormalizedRebindError::IdentityRecipe)?;
        if record.id != id {
            return Err(self.reject(corrupt()));
        }
        Ok(record)
    }
}

impl RdfDigestSource for OracleRdfDigestSource<'_, '_> {
    fn visit_neutral_ids(
        &mut self,
        visitor: &mut dyn FnMut(FactId, FactId) -> Result<(), NormalizedRebindError>,
    ) -> Option<Result<(), NormalizedRebindError>> {
        Some((|| {
            self.check()?;
            let mut heads = BinaryHeap::new();
            heads
                .try_reserve_exact(self.reader.rdf_partitions.len())
                .map_err(|_| NormalizedRebindError::ResourceLimit)?;
            for (owner, part) in self.reader.rdf_partitions.iter().enumerate() {
                if u64::try_from(part.neutral_ids().len()).ok()
                    != Some(part.descriptor().record_count())
                {
                    return Err(self.reject(corrupt()));
                }
                if let Some((original, neutral)) = part.neutral_ids().first() {
                    heads.push(Reverse((*original, *neutral, owner, 0usize)));
                }
            }
            let mut previous = None;
            let mut count = 0usize;
            while let Some(Reverse((original, neutral, owner, ordinal))) = heads.pop() {
                self.check()?;
                if previous.is_some_and(|old| old >= original) || count == self.count {
                    return Err(self.reject(corrupt()));
                }
                if let Err(error) = visitor(original, neutral) {
                    self.poisoned = true;
                    return Err(error);
                }
                previous = Some(original);
                count = count
                    .checked_add(1)
                    .ok_or(NormalizedRebindError::ResourceLimit)?;
                let next = ordinal
                    .checked_add(1)
                    .ok_or(NormalizedRebindError::ResourceLimit)?;
                if let Some((original, neutral)) = self
                    .reader
                    .rdf_partitions
                    .get(owner)
                    .and_then(|part| part.neutral_ids().get(next))
                {
                    heads.push(Reverse((*original, *neutral, owner, next)));
                }
            }
            if count != self.count {
                return Err(self.reject(corrupt()));
            }
            self.check()
        })())
    }

    fn visit(
        &mut self,
        visitor: &mut dyn FnMut(ExtensionEnvelope) -> Result<(), NormalizedRebindError>,
    ) -> Result<(), NormalizedRebindError> {
        self.check()?;
        let mut heads = BinaryHeap::new();
        heads
            .try_reserve_exact(self.reader.rdf_partitions.len())
            .map_err(|_| NormalizedRebindError::ResourceLimit)?;
        for owner in 0..self.reader.rdf_partitions.len() {
            if let Some(id) = self.head(owner, None)? {
                heads.push(Reverse((id, owner)));
            }
        }
        let mut count = 0usize;
        let mut previous = None;
        while let Some(Reverse((id, owner))) = heads.pop() {
            self.check()?;
            if previous.is_some_and(|old| old >= id) || count == self.count {
                return Err(self.reject(corrupt()));
            }
            let record = self.selected(owner, id)?;
            if let Err(error) = visitor(record) {
                self.poisoned = true;
                return Err(error);
            }
            count = count
                .checked_add(1)
                .ok_or(NormalizedRebindError::ResourceLimit)?;
            previous = Some(id);
            if let Some(next) = self.head(owner, Some(id))? {
                heads.push(Reverse((next, owner)));
            }
        }
        if count != self.count {
            return Err(self.reject(corrupt()));
        }
        self.check()
    }
    fn read(&mut self, id: FactId) -> Result<ExtensionEnvelope, NormalizedRebindError> {
        self.check()?;
        let mut owner = None;
        for index in 0..self.reader.rdf_partitions.len() {
            if self.head(index, predecessor(id))? == Some(id) && owner.replace(index).is_some() {
                return Err(self.reject(corrupt()));
            }
        }
        let owner = owner.ok_or_else(|| self.reject(corrupt()))?;
        self.selected(owner, id)
    }
}

fn predecessor(id: FactId) -> Option<FactId> {
    let mut bytes = *id.as_bytes();
    for byte in bytes.iter_mut().rev() {
        if let Some(previous) = byte.checked_sub(1) {
            *byte = previous;
            return Some(FactId::from_bytes(bytes));
        }
        *byte = u8::MAX;
    }
    None
}
fn corrupt() -> CatalogError {
    CatalogError::new(CatalogErrorKind::Corrupt)
}
