//! Opt-in, source-free timings for synchronous locate preparation and execution.

use std::{cell::RefCell, time::Instant};

use rootlight_query::ScopedTextProjectionObservation;
use serde::Serialize;

/// Numeric developer observations from one synchronous locate capture.
///
/// Nested phase durations overlap and must not be added as independent costs.
/// An operation error remains in the caller's result; `scope_completed` only
/// records that the capture returned. Repeated projection observations mark
/// telemetry incomplete because this fixed-size collector retains one detail.
#[doc(hidden)]
#[derive(Debug, Clone, Serialize)]
pub struct FirstSliceLocateProfile {
    pub(crate) scope_completed: bool,
    pub(crate) telemetry_complete: bool,
    pub(crate) wall_micros: u128,
    pub(crate) lease: Timing,
    pub(crate) query_construction: Timing,
    pub(crate) scoped_source: Timing,
    pub(crate) immutable_source_read: Timing,
    pub(crate) projection: Timing,
    pub(crate) ephemeral_build: Timing,
    pub(crate) planning: Timing,
    pub(crate) execution: Timing,
    pub(crate) projected_documents: Option<usize>,
    pub(crate) text_projection: Option<ScopedTextProjectionObservation>,
}

impl Default for FirstSliceLocateProfile {
    fn default() -> Self {
        Self {
            scope_completed: false,
            telemetry_complete: true,
            wall_micros: 0,
            lease: Timing::default(),
            query_construction: Timing::default(),
            scoped_source: Timing::default(),
            immutable_source_read: Timing::default(),
            projection: Timing::default(),
            ephemeral_build: Timing::default(),
            planning: Timing::default(),
            execution: Timing::default(),
            projected_documents: None,
            text_projection: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct Timing {
    pub(crate) scopes: u64,
    pub(crate) micros: u128,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Phase {
    Lease,
    QueryConstruction,
    ScopedSource,
    ImmutableSourceRead,
    Projection,
    EphemeralBuild,
    Planning,
    Execution,
}

thread_local! {
    static PROFILE: RefCell<Option<FirstSliceLocateProfile>> = const { RefCell::new(None) };
}

/// Captures numeric locate observations without changing its result or limits.
///
/// Disabled calls perform no clocks or extra source/record scans. Nested captures
/// restore the previous collector on return or unwind. Returned errors retain
/// partial observations; no observation asserts successful query completion.
///
/// # Panics
/// Propagates a panic from `work` after restoring the previous collector.
#[doc(hidden)]
pub fn with_first_slice_locate_profile<R>(
    work: impl FnOnce() -> R,
) -> (R, FirstSliceLocateProfile) {
    let previous =
        PROFILE.with(|profile| profile.replace(Some(FirstSliceLocateProfile::default())));
    let scope = Scope { previous };
    let started = Instant::now();
    let result = work();
    let mut profile = scope.finish();
    profile.scope_completed = true;
    profile.wall_micros = started.elapsed().as_micros();
    (result, profile)
}

struct Scope {
    previous: Option<FirstSliceLocateProfile>,
}

impl Scope {
    fn finish(self) -> FirstSliceLocateProfile {
        PROFILE.with(|profile| {
            profile
                .borrow_mut()
                .take()
                .unwrap_or_else(|| FirstSliceLocateProfile {
                    telemetry_complete: false,
                    ..FirstSliceLocateProfile::default()
                })
        })
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        PROFILE.with(|profile| profile.replace(self.previous.take()));
    }
}

pub(crate) fn enabled() -> bool {
    PROFILE.with(|profile| profile.borrow().is_some())
}

pub(crate) fn observe(phase: Phase) -> Guard {
    Guard {
        phase,
        started: enabled().then(Instant::now),
    }
}

pub(crate) struct Guard {
    phase: Phase,
    started: Option<Instant>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let Some(started) = self.started else { return };
        let elapsed = started.elapsed().as_micros();
        PROFILE.with(|profile| {
            let mut slot = profile.borrow_mut();
            let Some(profile) = slot.as_mut() else { return };
            let timing = match self.phase {
                Phase::Lease => &mut profile.lease,
                Phase::QueryConstruction => &mut profile.query_construction,
                Phase::ScopedSource => &mut profile.scoped_source,
                Phase::ImmutableSourceRead => &mut profile.immutable_source_read,
                Phase::Projection => &mut profile.projection,
                Phase::EphemeralBuild => &mut profile.ephemeral_build,
                Phase::Planning => &mut profile.planning,
                Phase::Execution => &mut profile.execution,
            };
            if let (Some(scopes), Some(micros)) = (
                timing.scopes.checked_add(1),
                timing.micros.checked_add(elapsed),
            ) {
                timing.scopes = scopes;
                timing.micros = micros;
            } else {
                profile.telemetry_complete = false;
            }
        });
    }
}

pub(crate) fn record_projection(observation: ScopedTextProjectionObservation) {
    PROFILE.with(|profile| {
        if let Some(profile) = profile.borrow_mut().as_mut() {
            if profile.text_projection.is_some() {
                profile.telemetry_complete = false;
            }
            profile.text_projection = Some(observation);
        }
    });
}

pub(crate) fn record_documents(count: usize) {
    PROFILE.with(|profile| {
        if let Some(profile) = profile.borrow_mut().as_mut() {
            if profile.projected_documents.is_some() {
                profile.telemetry_complete = false;
            }
            profile.projected_documents = Some(count);
        }
    });
}
