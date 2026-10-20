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
    /// Explicit source deadlines include generation and backing preparation;
    /// the implicit snapshot duration applies only to source work itself.
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
        budget: SourceFindBudget,
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
        if let Some(lease) = self.source_only_lease(generation, cancellation)? {
            let backing = crate::source_generation::SourceBacking {
                service: self,
                lease: &lease,
            };
            let source =
                rootlight_source::SourceService::from_source_authority(&backing, &lease.authority)
                    .map_err(|error| crate::map_source_error(error, cancellation))?;
            let budget = budget_after_literal_preparation(budget, true, started.elapsed())?;
            return source
                .find_literal(file, query, cursor, budget, cancellation)
                .map_err(FirstSliceSourceFindError::Find);
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
        let budget = budget_after_literal_preparation(
            budget,
            source.uses_verified_backing(),
            started.elapsed(),
        )?;
        source
            .find_literal(file.id, query, cursor, budget, cancellation)
            .map_err(FirstSliceSourceFindError::Find)
    }
}

fn budget_after_literal_preparation(
    mut budget: SourceFindBudget,
    verified_backing: bool,
    elapsed: Duration,
) -> Result<SourceFindBudget, SourceFindError> {
    if let Some(maximum) = budget.source.max_verification_duration {
        let remaining = remaining_literal_duration(maximum, elapsed)?;
        budget.source.max_verification_duration = Some(remaining);
        if !verified_backing {
            budget.source.max_duration = budget.source.max_duration.min(remaining);
        }
    }
    Ok(budget)
}

fn remaining_literal_duration(
    maximum: Duration,
    elapsed: Duration,
) -> Result<Duration, SourceFindError> {
    // An explicit caller deadline includes both source backings' preparation.
    // The legacy snapshot default is not a deadline for cold generation restore.
    maximum
        .checked_sub(elapsed)
        .filter(|remaining| !remaining.is_zero())
        .ok_or(SourceFindError::Source(SourceError::Cancelled(
            CancellationReason::DeadlineExceeded,
        )))
}

#[cfg(test)]
mod tests;
