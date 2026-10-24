//! Test-only, source-free first-failure evidence before Catalog error erasure.
//! A scoped thread-local observer changes no production error or cleanup path.

use std::{cell::Cell, io};

use rootlight_vfs::platform::{PlatformError, PublishError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Phase {
    Idle,
    StagePrepared,
    CancellationCheckpoint,
    DiscardAfterCancellation,
    CommitStaged,
    GenerationPublication,
    ActivationMarkerPublication,
    RepositoryCompaction,
    ActivationMarkerCompaction,
    SourceBlobCompaction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    Operation,
    NotCommitted,
    CommittedButDurabilityUnknown,
    OtherPublication,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Failure {
    pub(crate) phase: Phase,
    pub(crate) outcome: Outcome,
    pub(crate) category: &'static str,
    pub(crate) operation: &'static str,
    pub(crate) io_kind: Option<io::ErrorKind>,
    pub(crate) raw_os_error: Option<i32>,
}

#[derive(Clone, Copy)]
struct State {
    active: bool,
    phase: Phase,
    failure: Option<Failure>,
}

const IDLE: State = State {
    active: false,
    phase: Phase::Idle,
    failure: None,
};

thread_local! {
    static STATE: Cell<State> = const { Cell::new(IDLE) };
}

pub(crate) struct Observation(State);

pub(crate) fn observe() -> Observation {
    Observation(
        STATE
            .try_with(|state| {
                state.replace(State {
                    active: true,
                    ..IDLE
                })
            })
            .unwrap_or(IDLE),
    )
}

impl Observation {
    pub(crate) fn first_failure(&self) -> Option<Failure> {
        STATE.try_with(|state| state.get().failure).ok().flatten()
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        let _ = STATE.try_with(|state| state.set(self.0));
    }
}

pub(crate) struct PhaseGuard(Phase);

pub(crate) fn phase(phase: Phase) -> PhaseGuard {
    STATE
        .try_with(|state| {
            let mut current = state.get();
            let previous = current.phase;
            if current.active {
                current.phase = phase;
                state.set(current);
            }
            PhaseGuard(previous)
        })
        .unwrap_or_else(|_| PhaseGuard(Phase::Idle))
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        let _ = STATE.try_with(|state| {
            let mut current = state.get();
            if current.active {
                current.phase = self.0;
                state.set(current);
            }
        });
    }
}

fn record(
    outcome: Outcome,
    category: &'static str,
    operation: &'static str,
    error: Option<&io::Error>,
) {
    let _ = STATE.try_with(|state| {
        let mut current = state.get();
        if current.active && current.failure.is_none() {
            current.failure = Some(Failure {
                phase: current.phase,
                outcome,
                category,
                operation,
                io_kind: error.map(io::Error::kind),
                raw_os_error: error.and_then(io::Error::raw_os_error),
            });
            state.set(current);
        }
    });
}

pub(crate) fn io(error: &io::Error, operation: &'static str, outcome: Outcome) {
    record(outcome, "io", operation, Some(error));
}

pub(crate) fn service(operation: &'static str) {
    record(Outcome::Operation, "service-error", operation, None);
}

pub(crate) fn platform(error: &PlatformError, outcome: Outcome) {
    match error {
        PlatformError::Io { operation, source } => io(source, operation, outcome),
        PlatformError::InvalidName => record(outcome, "invalid-name", "private-tree", None),
        PlatformError::InsecureParent => record(outcome, "insecure-parent", "private-tree", None),
        PlatformError::SecurityPolicy => record(outcome, "security-policy", "private-tree", None),
        PlatformError::ResourceLimit => record(outcome, "resource-limit", "private-tree", None),
        PlatformError::Cancelled(_) => record(outcome, "cancelled", "private-tree", None),
        PlatformError::UnsupportedPlatform => {
            record(outcome, "unsupported-platform", "private-tree", None)
        }
        _ => record(outcome, "other-platform", "private-tree", None),
    }
}

pub(crate) fn publication(error: &PublishError) {
    match error {
        PublishError::NotCommitted { source } => platform(source, Outcome::NotCommitted),
        PublishError::CommittedButDurabilityUnknown { source, .. } => {
            io(
                source,
                "publish-private-directory-post-commit",
                Outcome::CommittedButDurabilityUnknown,
            );
        }
        _ => record(
            Outcome::OtherPublication,
            "other-publication",
            "publish",
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_observer_ignores_failures() {
        io(
            &io::Error::from_raw_os_error(5),
            "write",
            Outcome::Operation,
        );
        assert!(STATE.with(|state| state.get().failure).is_none());
    }

    #[test]
    fn first_failure_survives_cleanup_error_and_retains_exact_source_free_fields() {
        let observed = observe();
        let phase = phase(Phase::GenerationPublication);
        let first = io::Error::from_raw_os_error(5);
        io(&first, "rename", Outcome::NotCommitted);
        io(
            &io::Error::from_raw_os_error(32),
            "cleanup",
            Outcome::Operation,
        );
        assert_eq!(
            observed.first_failure(),
            Some(Failure {
                phase: Phase::GenerationPublication,
                outcome: Outcome::NotCommitted,
                category: "io",
                operation: "rename",
                io_kind: Some(first.kind()),
                raw_os_error: Some(5),
            })
        );
        drop(phase);
        drop(observed);
        assert!(!STATE.with(|state| state.get().active));
    }

    #[test]
    fn nested_phase_restores_outer_context_and_next_observation_starts_empty() {
        let observed = observe();
        let outer = phase(Phase::CommitStaged);
        {
            let _inner = phase(Phase::ActivationMarkerCompaction);
        }
        platform(&PlatformError::SecurityPolicy, Outcome::Operation);
        assert_eq!(observed.first_failure().unwrap().phase, Phase::CommitStaged);
        drop(outer);
        drop(observed);
        let next = observe();
        assert!(next.first_failure().is_none());
    }

    #[test]
    fn publish_outcomes_remain_distinct_and_expose_numeric_io_cause() {
        let observed = observe();
        let _phase = phase(Phase::ActivationMarkerPublication);
        publication(&PublishError::NotCommitted {
            source: PlatformError::Io {
                operation: "rename_private",
                source: io::Error::from_raw_os_error(32),
            },
        });
        let first = observed.first_failure().unwrap();
        assert_eq!(first.outcome, Outcome::NotCommitted);
        assert_eq!(first.operation, "rename_private");
        assert_eq!(first.raw_os_error, Some(32));
    }

    #[test]
    fn committed_durability_cause_is_not_replaced_by_cleanup_or_service_fallback() {
        let observed = observe();
        let _phase = phase(Phase::GenerationPublication);
        let error = io::Error::from_raw_os_error(32);
        io(
            &error,
            "publish-private-directory-post-commit",
            Outcome::CommittedButDurabilityUnknown,
        );
        platform(&PlatformError::SecurityPolicy, Outcome::Operation);
        service("stage-prepared");
        let first = observed.first_failure().unwrap();
        assert_eq!(first.outcome, Outcome::CommittedButDurabilityUnknown);
        assert_eq!(first.raw_os_error, Some(32));
        assert_eq!(first.operation, "publish-private-directory-post-commit");
    }

    #[test]
    fn separate_threads_cannot_occupy_each_others_first_failure() {
        let observed = observe();
        let other = std::thread::spawn(|| {
            io(
                &io::Error::from_raw_os_error(5),
                "unarmed",
                Outcome::Operation,
            );
            let observed = observe();
            let _phase = phase(Phase::ActivationMarkerPublication);
            io(
                &io::Error::from_raw_os_error(32),
                "other-thread",
                Outcome::Operation,
            );
            observed.first_failure().unwrap()
        })
        .join()
        .unwrap();
        assert_eq!(other.raw_os_error, Some(32));
        assert!(observed.first_failure().is_none());
        service("commit-staged");
        assert_eq!(observed.first_failure().unwrap().operation, "commit-staged");
    }
}
