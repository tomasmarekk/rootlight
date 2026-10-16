//! Direct generation-pinned literal search without a lexical query plan.
//!
//! The generation lease authorizes snapshots and private verified backings just
//! as exact reads do; the source service owns matching and byte evidence.

use std::time::{Duration, Instant};

use rootlight_cancel::CancellationReason;
use rootlight_source::{
    LiteralSearchError, SourceError, SourceFindBudget, SourceFindCursor, SourceFindError,
    SourceFindResult,
};

use super::{
    Cancellation, FileId, FirstSliceError, FirstSliceService, GenerationId, SourceRef, SourceSpan,
    check_cancellation,
};

/// Preserves service admission separately from precise source-search failures.
#[derive(Debug, thiserror::Error)]
pub enum FirstSliceSourceFindError {
    /// Generation retention, durable hydration, or service cancellation failed.
    #[error(transparent)]
    Service(#[from] FirstSliceError),
    /// Source identity, literal query, cursor, or bounded search failed.
    #[error(transparent)]
    Find(#[from] SourceFindError),
}

impl FirstSliceService {
    /// Finds an exact literal directly in one immutable generation-owned file.
    ///
    /// Match spans and continuation coverage come from verified source bytes.
    /// No lexical projection, query plan, or inferred total is constructed.
    ///
    /// # Errors
    /// Returns source identity, generation, cancellation, or resource failures.
    /// Malformed continuation parts cannot produce a partial response.
    pub fn find_literal(
        &self,
        generation: GenerationId,
        file: FileId,
        query: &str,
        cursor: Option<SourceFindCursor>,
        mut budget: SourceFindBudget,
        cancellation: &Cancellation,
    ) -> Result<SourceFindResult, FirstSliceSourceFindError> {
        let started = Instant::now();
        check_cancellation(cancellation)?;
        budget.validate()?;
        if query.is_empty() {
            return Err(SourceFindError::Literal(LiteralSearchError::EmptyQuery).into());
        }
        if query.len() > budget.matcher.max_query_bytes {
            return Err(SourceFindError::Literal(LiteralSearchError::QueryLimit).into());
        }
        let lease = self.generation_lease(generation, cancellation)?;
        let snapshot = lease.generation();
        let file = snapshot
            .find_file(file)
            .ok_or(SourceFindError::Source(SourceError::FileNotFound))?;
        let identity = SourceRef::new(
            snapshot.metadata().repository(),
            generation,
            SourceSpan::new(file.id, 0, 0)
                .map_err(|_| SourceFindError::Source(SourceError::InvalidSourceSpan))?,
            file.content_hash,
            None,
        );
        let sources =
            self.resolve_source_backing(snapshot, std::slice::from_ref(&identity), cancellation)?;
        let source = sources.service(snapshot, cancellation)?;
        if !source.uses_verified_backing() && file.byte_length > budget.source.max_snapshot_bytes {
            return Err(SourceFindError::Source(SourceError::SnapshotBudgetExceeded).into());
        }
        check_cancellation(cancellation)?;
        if source.uses_verified_backing() {
            budget.source.max_verification_duration = budget
                .source
                .max_verification_duration
                .map(|maximum| remaining_literal_duration(maximum, started.elapsed()))
                .transpose()?;
        } else {
            budget.source.max_duration =
                remaining_literal_duration(budget.source.max_duration, started.elapsed())?;
        }
        source
            .find_literal(file.id, query, cursor, budget, cancellation)
            .map_err(FirstSliceSourceFindError::Find)
    }
}

fn remaining_literal_duration(
    maximum: Duration,
    elapsed: Duration,
) -> Result<Duration, SourceFindError> {
    // Resolving either source backing belongs to the same source operation.
    // Starting a fresh timer here would escape the caller's source deadline.
    maximum
        .checked_sub(elapsed)
        .filter(|remaining| !remaining.is_zero())
        .ok_or(SourceFindError::Source(SourceError::Cancelled(
            CancellationReason::DeadlineExceeded,
        )))
}

#[cfg(test)]
mod tests;
