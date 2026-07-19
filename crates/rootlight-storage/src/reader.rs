//! Backend-neutral indexed reads over one immutable generation.
//!
//! Requests own validated limits and pages use stable fact identifiers rather
//! than backend offsets, so SQLite and future readers expose equal semantics.

use std::error::Error;

use rootlight_ids::{FactId, FileId, SymbolId};
use rootlight_ir::{
    CoverageRecord, CoverageScope, EntityRecord, FileRecord, OccurrenceRecord, ProvenanceRecord,
    RelationEndpoint, RelationPredicate, RelationRecord,
};

use crate::{GenerationContext, GenerationMetadata, GenerationStats, IdentityVerifiedGeneration};

/// Hard ceiling for records returned by one indexed generation read.
pub const HARD_MAX_GENERATION_READ_ITEMS: u16 = 4_096;

const DEFAULT_MAX_GENERATION_READ_ITEMS: u16 = 256;
const HARD_MAX_RELATION_PREDICATES: usize = 64;

/// A validated maximum number of records returned by one indexed read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationReadLimit(u16);

impl GenerationReadLimit {
    /// Creates a positive read limit within the backend-neutral hard ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationReadLimitError`] when `maximum` is zero or exceeds
    /// [`HARD_MAX_GENERATION_READ_ITEMS`].
    pub fn new(maximum: u32) -> Result<Self, GenerationReadLimitError> {
        let maximum =
            u16::try_from(maximum).map_err(|_| GenerationReadLimitError::AboveHardLimit {
                maximum: HARD_MAX_GENERATION_READ_ITEMS,
            })?;
        if maximum == 0 {
            return Err(GenerationReadLimitError::Zero);
        }
        if maximum > HARD_MAX_GENERATION_READ_ITEMS {
            return Err(GenerationReadLimitError::AboveHardLimit {
                maximum: HARD_MAX_GENERATION_READ_ITEMS,
            });
        }
        Ok(Self(maximum))
    }

    /// Returns the validated maximum item count.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl Default for GenerationReadLimit {
    fn default() -> Self {
        Self(DEFAULT_MAX_GENERATION_READ_ITEMS)
    }
}

/// Invalid caller-supplied indexed read limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GenerationReadLimitError {
    /// The requested page size was zero.
    #[error("generation read limit must be positive")]
    Zero,
    /// The requested page size exceeded the backend-neutral ceiling.
    #[error("generation read limit exceeds the hard limit")]
    AboveHardLimit {
        /// Largest accepted item count.
        maximum: u16,
    },
}

/// Whether an indexed read reached the end of its matching stable-ID range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadPageCompleteness {
    /// No matching record remains after the returned page.
    Complete,
    /// At least one matching record remains after the returned page.
    Truncated,
}

/// One owned, bounded page ordered by stable fact identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPage<T> {
    items: Vec<T>,
    total_available: u64,
    completeness: ReadPageCompleteness,
    next_cursor: Option<FactId>,
}

impl<T> ReadPage<T> {
    /// Constructs a page from a backend's `LIMIT + 1` result.
    ///
    /// The identifier callback must return the stable fact ID used by the
    /// backend's ascending order. The extra record, when present, is discarded
    /// and only proves that the page is truncated.
    ///
    /// # Errors
    ///
    /// Returns [`ReadPageError`] when the backend returned more than one probe
    /// record, reported an impossible total, or did not use strictly ascending
    /// stable IDs.
    pub fn from_limit_plus_one(
        mut items: Vec<T>,
        total_available: u64,
        limit: GenerationReadLimit,
        stable_id: impl Fn(&T) -> FactId,
    ) -> Result<Self, ReadPageError> {
        let maximum = usize::from(limit.get());
        let probe_maximum = maximum.checked_add(1).ok_or(ReadPageError::TooManyRows {
            maximum: limit.get(),
        })?;
        if items.len() > probe_maximum {
            return Err(ReadPageError::TooManyRows {
                maximum: limit.get(),
            });
        }
        let materialized =
            u64::try_from(items.len()).map_err(|_| ReadPageError::TotalBelowMaterialized)?;
        if total_available < materialized {
            return Err(ReadPageError::TotalBelowMaterialized);
        }
        if items
            .windows(2)
            .any(|pair| stable_id(&pair[0]) >= stable_id(&pair[1]))
        {
            return Err(ReadPageError::UnstableOrder);
        }

        if items.len() > maximum {
            items.truncate(maximum);
            let next_cursor = items.last().map(stable_id);
            Ok(Self {
                items,
                total_available,
                completeness: ReadPageCompleteness::Truncated,
                next_cursor,
            })
        } else {
            Ok(Self {
                items,
                total_available,
                completeness: ReadPageCompleteness::Complete,
                next_cursor: None,
            })
        }
    }

    /// Returns the owned records in ascending stable-ID order.
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Returns all records matching the request independently of its cursor.
    #[must_use]
    pub const fn total_available(&self) -> u64 {
        self.total_available
    }

    /// Returns whether another page exists after this page.
    #[must_use]
    pub const fn completeness(&self) -> ReadPageCompleteness {
        self.completeness
    }

    /// Returns the last emitted stable fact ID when another page exists.
    #[must_use]
    pub const fn next_cursor(&self) -> Option<FactId> {
        self.next_cursor
    }

    /// Consumes the page into its owned records.
    #[must_use]
    pub fn into_items(self) -> Vec<T> {
        self.items
    }
}

/// Invalid output from a backend's bounded page query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReadPageError {
    /// The backend returned more than the requested page and one probe row.
    #[error("generation read returned too many rows")]
    TooManyRows {
        /// Requested page size before the one-row probe.
        maximum: u16,
    },
    /// The backend's total was smaller than the materialized page.
    #[error("generation read total is smaller than its materialized rows")]
    TotalBelowMaterialized,
    /// Rows were not strictly ordered by stable fact identifier.
    #[error("generation read rows are not in stable identifier order")]
    UnstableOrder,
}

/// Direction of relations anchored at a typed endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationReadDirection {
    /// Relations whose subject is the anchor.
    Outgoing,
    /// Relations whose object is the anchor.
    Incoming,
}

/// A bounded relation read anchored at one typed endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationReadRequest {
    anchor: RelationEndpoint,
    direction: RelationReadDirection,
    predicates: Vec<RelationPredicate>,
    after: Option<FactId>,
    limit: GenerationReadLimit,
}

impl RelationReadRequest {
    /// Creates an unfiltered relation read from the start of the stable-ID range.
    #[must_use]
    pub fn new(
        anchor: RelationEndpoint,
        direction: RelationReadDirection,
        limit: GenerationReadLimit,
    ) -> Self {
        Self {
            anchor,
            direction,
            predicates: Vec::new(),
            after: None,
            limit,
        }
    }

    /// Restricts the read to a bounded canonical set of predicates.
    ///
    /// An empty vector retains the unfiltered behavior. Duplicate predicates
    /// collapse to one semantic filter.
    ///
    /// # Errors
    ///
    /// Returns [`RelationReadRequestError`] when the supplied vector exceeds
    /// the request hard ceiling before canonicalization.
    pub fn with_predicates(
        mut self,
        mut predicates: Vec<RelationPredicate>,
    ) -> Result<Self, RelationReadRequestError> {
        if predicates.len() > HARD_MAX_RELATION_PREDICATES {
            return Err(RelationReadRequestError::TooManyPredicates {
                maximum: HARD_MAX_RELATION_PREDICATES,
            });
        }
        predicates.sort_unstable();
        predicates.dedup();
        self.predicates = predicates;
        Ok(self)
    }

    /// Continues strictly after a previously returned stable fact ID.
    #[must_use]
    pub fn with_after(mut self, after: FactId) -> Self {
        self.after = Some(after);
        self
    }

    /// Returns the typed endpoint anchoring the read.
    #[must_use]
    pub const fn anchor(&self) -> RelationEndpoint {
        self.anchor
    }

    /// Returns whether the anchor is matched as subject or object.
    #[must_use]
    pub const fn direction(&self) -> RelationReadDirection {
        self.direction
    }

    /// Returns the canonical predicate filter, empty when all predicates match.
    #[must_use]
    pub fn predicates(&self) -> &[RelationPredicate] {
        &self.predicates
    }

    /// Returns the exclusive stable-ID cursor.
    #[must_use]
    pub const fn after(&self) -> Option<FactId> {
        self.after
    }

    /// Returns the validated page size.
    #[must_use]
    pub const fn limit(&self) -> GenerationReadLimit {
        self.limit
    }
}

/// Invalid relation read filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RelationReadRequestError {
    /// The raw predicate list exceeded its hard ceiling.
    #[error("relation read has too many predicates")]
    TooManyPredicates {
        /// Largest accepted raw predicate count.
        maximum: usize,
    },
}

/// A bounded occurrence read for one immutable file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OccurrenceReadRequest {
    file: FileId,
    after: Option<FactId>,
    limit: GenerationReadLimit,
}

impl OccurrenceReadRequest {
    /// Creates an occurrence read from the start of the stable-ID range.
    #[must_use]
    pub const fn new(file: FileId, limit: GenerationReadLimit) -> Self {
        Self {
            file,
            after: None,
            limit,
        }
    }

    /// Continues strictly after a previously returned stable fact ID.
    #[must_use]
    pub const fn with_after(mut self, after: FactId) -> Self {
        self.after = Some(after);
        self
    }

    /// Returns the file whose occurrences are requested.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// Returns the exclusive stable-ID cursor.
    #[must_use]
    pub const fn after(&self) -> Option<FactId> {
        self.after
    }

    /// Returns the validated page size.
    #[must_use]
    pub const fn limit(&self) -> GenerationReadLimit {
        self.limit
    }
}

/// A bounded coverage read for one repository, file, or entity scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageReadRequest {
    scope: CoverageScope,
    after: Option<FactId>,
    limit: GenerationReadLimit,
}

impl CoverageReadRequest {
    /// Creates a coverage read from the start of the stable-ID range.
    #[must_use]
    pub const fn new(scope: CoverageScope, limit: GenerationReadLimit) -> Self {
        Self {
            scope,
            after: None,
            limit,
        }
    }

    /// Continues strictly after a previously returned stable fact ID.
    #[must_use]
    pub const fn with_after(mut self, after: FactId) -> Self {
        self.after = Some(after);
        self
    }

    /// Returns the scope whose coverage records are requested.
    #[must_use]
    pub const fn scope(&self) -> CoverageScope {
        self.scope
    }

    /// Returns the exclusive stable-ID cursor.
    #[must_use]
    pub const fn after(&self) -> Option<FactId> {
        self.after
    }

    /// Returns the validated page size.
    #[must_use]
    pub const fn limit(&self) -> GenerationReadLimit {
        self.limit
    }
}

/// A backend-neutral reader for one immutable, pinned generation.
pub trait GenerationReader: Send + Sync {
    /// Typed backend error with a source-redacted display contract.
    type Error: Error + Send + Sync + 'static;

    /// Returns the pinned generation metadata.
    fn metadata(&self) -> GenerationMetadata;

    /// Returns verified generation cardinalities.
    fn stats(&self) -> GenerationStats;

    /// Reads one owned file record by stable identifier.
    ///
    /// # Errors
    ///
    /// Returns the backend error for cancellation, budget, compatibility,
    /// corruption, or storage failures.
    fn file(
        &self,
        id: FileId,
        context: &GenerationContext<'_>,
    ) -> Result<Option<FileRecord>, Self::Error>;

    /// Reads one owned entity record by stable identifier.
    ///
    /// # Errors
    ///
    /// Returns the backend error for cancellation, budget, compatibility,
    /// corruption, or storage failures.
    fn entity(
        &self,
        id: SymbolId,
        context: &GenerationContext<'_>,
    ) -> Result<Option<EntityRecord>, Self::Error>;

    /// Reads relations in ascending stable-ID order.
    ///
    /// # Errors
    ///
    /// Returns the backend error for cancellation, budget, compatibility,
    /// corruption, or storage failures.
    fn relations(
        &self,
        request: &RelationReadRequest,
        context: &GenerationContext<'_>,
    ) -> Result<ReadPage<RelationRecord>, Self::Error>;

    /// Reads file occurrences in ascending stable-ID order.
    ///
    /// # Errors
    ///
    /// Returns the backend error for cancellation, budget, compatibility,
    /// corruption, or storage failures.
    fn occurrences(
        &self,
        request: &OccurrenceReadRequest,
        context: &GenerationContext<'_>,
    ) -> Result<ReadPage<OccurrenceRecord>, Self::Error>;

    /// Reads one owned provenance record by stable identifier.
    ///
    /// # Errors
    ///
    /// Returns the backend error for cancellation, budget, compatibility,
    /// corruption, or storage failures.
    fn provenance(
        &self,
        id: FactId,
        context: &GenerationContext<'_>,
    ) -> Result<Option<ProvenanceRecord>, Self::Error>;

    /// Reads coverage records in ascending stable-ID order.
    ///
    /// # Errors
    ///
    /// Returns the backend error for cancellation, budget, compatibility,
    /// corruption, or storage failures.
    fn coverage(
        &self,
        request: &CoverageReadRequest,
        context: &GenerationContext<'_>,
    ) -> Result<ReadPage<CoverageRecord>, Self::Error>;

    /// Materializes the complete identity-verified generation.
    ///
    /// This compatibility and diagnostic path is not the indexed query path.
    ///
    /// # Errors
    ///
    /// Returns the backend error for cancellation, budget, compatibility,
    /// corruption, or storage failures.
    fn read_generation(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<IdentityVerifiedGeneration, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(byte: u8) -> FactId {
        FactId::from_bytes([byte; 20])
    }

    #[test]
    fn read_limit_rejects_zero_and_values_above_the_hard_ceiling() {
        assert_eq!(
            GenerationReadLimit::new(0),
            Err(GenerationReadLimitError::Zero)
        );
        assert_eq!(
            GenerationReadLimit::new(u32::from(HARD_MAX_GENERATION_READ_ITEMS) + 1),
            Err(GenerationReadLimitError::AboveHardLimit {
                maximum: HARD_MAX_GENERATION_READ_ITEMS
            })
        );
    }

    #[test]
    fn limit_plus_one_page_uses_the_last_emitted_stable_id_as_cursor() {
        let limit = GenerationReadLimit::new(2).expect("fixture limit is valid");
        let page =
            ReadPage::from_limit_plus_one(vec![fact(1), fact(2), fact(3)], 3, limit, |id| *id)
                .expect("ordered probe page is valid");

        assert_eq!(page.items(), &[fact(1), fact(2)]);
        assert_eq!(page.total_available(), 3);
        assert_eq!(page.completeness(), ReadPageCompleteness::Truncated);
        assert_eq!(page.next_cursor(), Some(fact(2)));
    }

    #[test]
    fn page_rejects_unstable_identifier_order() {
        let limit = GenerationReadLimit::new(2).expect("fixture limit is valid");
        assert_eq!(
            ReadPage::from_limit_plus_one(vec![fact(2), fact(1)], 2, limit, |id| *id),
            Err(ReadPageError::UnstableOrder)
        );
    }

    #[test]
    fn relation_predicates_are_canonical_and_deduplicated() {
        let request = RelationReadRequest::new(
            RelationEndpoint::Occurrence(fact(9)),
            RelationReadDirection::Outgoing,
            GenerationReadLimit::default(),
        )
        .with_predicates(vec![
            RelationPredicate::Calls,
            RelationPredicate::Contains,
            RelationPredicate::Calls,
        ])
        .expect("fixture predicate set is bounded");

        assert_eq!(
            request.predicates(),
            &[RelationPredicate::Contains, RelationPredicate::Calls]
        );
    }

    #[test]
    fn request_cursors_are_exclusive_stable_fact_ids() {
        let file = FileId::from_bytes([7; 20]);
        let scope = CoverageScope::File(file);
        let after = fact(8);
        let limit = GenerationReadLimit::new(1).expect("fixture limit is valid");

        assert_eq!(
            OccurrenceReadRequest::new(file, limit)
                .with_after(after)
                .after(),
            Some(after)
        );
        assert_eq!(
            CoverageReadRequest::new(scope, limit)
                .with_after(after)
                .after(),
            Some(after)
        );
    }
}
