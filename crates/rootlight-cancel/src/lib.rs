//! Cooperative cancellation contracts for bounded synchronous work.
//!
//! The token is cheap to clone, monotonic, first-reason-wins, and independent
//! of async runtimes. Future transport crates may bridge it without changing
//! analysis-domain APIs.

#![forbid(unsafe_code)]

use std::{
    fmt,
    sync::{Arc, OnceLock},
    time::Instant,
};

use serde::{Deserialize, Serialize};

/// The closed cancellation reasons safe to expose across internal boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CancellationReason {
    /// A client explicitly requested cancellation.
    ClientRequest,
    /// A parent operation was cancelled.
    ParentCancelled,
    /// A monotonic deadline elapsed.
    DeadlineExceeded,
    /// The owning process is shutting down.
    Shutdown,
    /// A declared resource limit was reached.
    ResourceLimit,
}

/// A cooperative cancellation token with an optional monotonic deadline.
#[derive(Clone)]
pub struct Cancellation {
    inner: Arc<Inner>,
}

impl Cancellation {
    /// Creates an uncancelled token without a deadline.
    #[must_use]
    pub fn new() -> Self {
        Self::with_optional_deadline(None)
    }

    /// Creates an uncancelled token with a monotonic deadline.
    #[must_use]
    pub fn with_deadline(deadline: Instant) -> Self {
        Self::with_optional_deadline(Some(deadline))
    }

    fn with_optional_deadline(deadline: Option<Instant>) -> Self {
        Self {
            inner: Arc::new(Inner {
                reason: OnceLock::new(),
                deadline,
            }),
        }
    }

    /// Records cancellation when no earlier reason won.
    ///
    /// Returns `true` only for the caller that establishes the terminal reason.
    pub fn cancel(&self, reason: CancellationReason) -> bool {
        self.inner.reason.set(reason).is_ok()
    }

    /// Returns the first recorded cancellation reason, observing deadlines.
    #[must_use]
    pub fn reason(&self) -> Option<CancellationReason> {
        self.reason_at(Instant::now())
    }

    /// Returns the cancellation reason at a supplied monotonic instant.
    ///
    /// This deterministic form lets tests and bounded loops avoid sleeping.
    #[must_use]
    pub fn reason_at(&self, now: Instant) -> Option<CancellationReason> {
        if let Some(reason) = self.inner.reason.get() {
            return Some(*reason);
        }
        if self.inner.deadline.is_some_and(|deadline| now >= deadline) {
            let _ = self.inner.reason.set(CancellationReason::DeadlineExceeded);
        }
        self.inner.reason.get().copied()
    }

    /// Checks whether work should continue.
    ///
    /// # Errors
    ///
    /// Returns [`Cancelled`] after explicit cancellation or deadline expiry.
    pub fn check(&self) -> Result<(), Cancelled> {
        self.check_at(Instant::now())
    }

    /// Checks cancellation at a supplied monotonic instant.
    ///
    /// # Errors
    ///
    /// Returns [`Cancelled`] when a terminal reason has been established.
    pub fn check_at(&self, now: Instant) -> Result<(), Cancelled> {
        match self.reason_at(now) {
            Some(reason) => Err(Cancelled { reason }),
            None => Ok(()),
        }
    }
}

impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Cancellation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Cancellation")
            .field("reason", &self.inner.reason.get())
            .field("has_deadline", &self.inner.deadline.is_some())
            .finish()
    }
}

#[derive(Debug)]
struct Inner {
    reason: OnceLock<CancellationReason>,
    deadline: Option<Instant>,
}

/// Terminal cooperative cancellation returned by checkpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("operation was cancelled: {reason:?}")]
pub struct Cancelled {
    reason: CancellationReason,
}

impl Cancelled {
    /// Returns the stable first-writer cancellation reason.
    #[must_use]
    pub const fn reason(self) -> CancellationReason {
        self.reason
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            Arc, Barrier,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::Duration,
    };

    #[test]
    fn clones_observe_monotonic_first_reason() {
        let cancellation = Cancellation::new();
        let clone = cancellation.clone();

        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(!clone.cancel(CancellationReason::Shutdown));
        assert_eq!(clone.reason(), Some(CancellationReason::ClientRequest));
    }

    #[test]
    fn deadline_activates_at_its_instant() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(10);
        let cancellation = Cancellation::with_deadline(deadline);

        assert_eq!(
            cancellation.check_at(deadline - Duration::from_nanos(1)),
            Ok(())
        );
        assert_eq!(
            cancellation.check_at(deadline),
            Err(Cancelled {
                reason: CancellationReason::DeadlineExceeded
            })
        );
    }

    #[test]
    fn concurrent_cancellation_records_exactly_one_winner() {
        let cancellation = Cancellation::new();
        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for reason in [
            CancellationReason::ClientRequest,
            CancellationReason::Shutdown,
        ] {
            let token = cancellation.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                token.cancel(reason)
            }));
        }
        barrier.wait();
        let winners = handles
            .into_iter()
            .map(|handle| handle.join().expect("worker does not panic"))
            .filter(|won| *won)
            .count();

        assert_eq!(winners, 1);
        assert!(cancellation.reason().is_some());
    }

    #[test]
    fn worker_reaches_cleanup_and_joins_after_cancellation() {
        let cancellation = Cancellation::new();
        let cleaned = Arc::new(AtomicBool::new(false));
        let worker_token = cancellation.clone();
        let worker_cleaned = Arc::clone(&cleaned);
        let worker = thread::spawn(move || {
            struct Cleanup(Arc<AtomicBool>);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _cleanup = Cleanup(worker_cleaned);
            while worker_token.check().is_ok() {
                thread::yield_now();
            }
        });

        cancellation.cancel(CancellationReason::ClientRequest);
        worker.join().expect("worker exits after cancellation");
        assert!(cleaned.load(Ordering::SeqCst));
    }
}
