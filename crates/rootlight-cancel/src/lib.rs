//! Cooperative cancellation contracts for synchronous and asynchronous work.
//!
//! TASK-01.3 adds the monotonic token and deadline semantics after the workspace
//! boundary is established.

#![forbid(unsafe_code)]

/// The closed cancellation reasons safe to expose across internal boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}
