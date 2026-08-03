//! Session-owned browser handles for retained daemon graph projections.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use data_encoding::BASE64URL_NOPAD;
use rootlight_client::{GraphProjectionContinuation, GraphProjectionId, GraphProjectionPage};
use sha2::Digest as _;
use subtle::ConstantTimeEq as _;
use tokio::sync::Mutex as AsyncMutex;

use crate::session::SessionIdentity;

const TOKEN_BYTES: usize = 32;
const TOKEN_LENGTH: usize = 43;
const MAX_PROJECTIONS_PER_SESSION: usize = 8;
const MAX_PROJECTIONS_GLOBAL: usize = 64;
const PROJECTION_IDLE_TTL: Duration = Duration::from_secs(60);
const PROJECTION_ABSOLUTE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum GraphRegistryError {
    #[error("graph projection handle is invalid")]
    Invalid,
    #[error("graph projection capacity is unavailable")]
    LimitReached,
    #[error("graph projection registry is unavailable")]
    ResourceUnavailable,
    #[error("graph projection page is already being requested")]
    Busy,
    #[error("graph projection has no continuation")]
    Exhausted,
    #[error("graph projection page ordinal overflowed")]
    OrdinalOverflow,
}

pub(crate) struct GraphProjectionHandle {
    projection: GraphProjectionId,
    progress: AsyncMutex<ProjectionProgress>,
}

struct ProjectionProgress {
    continuation: Option<GraphProjectionContinuation>,
    next_page_ordinal: u32,
    busy: bool,
}

impl GraphProjectionHandle {
    fn new(page: &GraphProjectionPage) -> Self {
        Self {
            projection: page.projection,
            progress: AsyncMutex::new(ProjectionProgress {
                continuation: page.continuation.clone(),
                next_page_ordinal: 1,
                busy: false,
            }),
        }
    }

    pub(crate) const fn projection(&self) -> GraphProjectionId {
        self.projection
    }

    pub(crate) async fn begin_next(
        &self,
    ) -> Result<(GraphProjectionContinuation, u32), GraphRegistryError> {
        let mut progress = self.progress.lock().await;
        if progress.busy {
            return Err(GraphRegistryError::Busy);
        }
        let continuation = progress
            .continuation
            .take()
            .ok_or(GraphRegistryError::Exhausted)?;
        progress.busy = true;
        Ok((continuation, progress.next_page_ordinal))
    }

    pub(crate) async fn finish_next(
        &self,
        page: &GraphProjectionPage,
    ) -> Result<(), GraphRegistryError> {
        let mut progress = self.progress.lock().await;
        progress.next_page_ordinal = progress
            .next_page_ordinal
            .checked_add(1)
            .ok_or(GraphRegistryError::OrdinalOverflow)?;
        progress.continuation = page.continuation.clone();
        progress.busy = false;
        Ok(())
    }

    pub(crate) async fn abandon_next(&self) {
        let mut progress = self.progress.lock().await;
        progress.continuation = None;
        progress.busy = false;
    }
}

pub(crate) struct GraphRegistry {
    state: Mutex<RegistryState>,
}

struct RegistryState {
    records: VecDeque<ProjectionRecord>,
}

struct ProjectionRecord {
    owner: SessionIdentity,
    token_digest: [u8; 32],
    created_at: Instant,
    last_seen: Instant,
    handle: Arc<GraphProjectionHandle>,
}

pub(crate) struct IssuedGraphProjection {
    pub(crate) token: String,
}

impl GraphRegistry {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                records: VecDeque::new(),
            }),
        }
    }

    pub(crate) fn issue(
        &self,
        owner: SessionIdentity,
        page: &GraphProjectionPage,
        now: Instant,
    ) -> Result<IssuedGraphProjection, GraphRegistryError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| GraphRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        let owned = state
            .records
            .iter()
            .filter(|record| record.owner == owner)
            .count();
        if owned >= MAX_PROJECTIONS_PER_SESSION || state.records.len() >= MAX_PROJECTIONS_GLOBAL {
            return Err(GraphRegistryError::LimitReached);
        }
        let (token, token_digest) = random_token()?;
        let handle = Arc::new(GraphProjectionHandle::new(page));
        state.records.push_back(ProjectionRecord {
            owner,
            token_digest,
            created_at: now,
            last_seen: now,
            handle: Arc::clone(&handle),
        });
        Ok(IssuedGraphProjection { token })
    }

    pub(crate) fn claim(
        &self,
        owner: SessionIdentity,
        token: &str,
        now: Instant,
    ) -> Result<Arc<GraphProjectionHandle>, GraphRegistryError> {
        let digest = decode_token_digest(token)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| GraphRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        let record = state
            .records
            .iter_mut()
            .find(|record| {
                record.owner == owner && bool::from(record.token_digest.ct_eq(digest.as_slice()))
            })
            .ok_or(GraphRegistryError::Invalid)?;
        record.last_seen = now;
        Ok(Arc::clone(&record.handle))
    }

    pub(crate) fn release(
        &self,
        owner: SessionIdentity,
        token: &str,
    ) -> Result<Arc<GraphProjectionHandle>, GraphRegistryError> {
        let digest = decode_token_digest(token)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| GraphRegistryError::ResourceUnavailable)?;
        let index = state
            .records
            .iter()
            .position(|record| {
                record.owner == owner && bool::from(record.token_digest.ct_eq(digest.as_slice()))
            })
            .ok_or(GraphRegistryError::Invalid)?;
        Ok(state
            .records
            .remove(index)
            .ok_or(GraphRegistryError::ResourceUnavailable)?
            .handle)
    }

    pub(crate) fn clear_session(
        &self,
        owner: SessionIdentity,
    ) -> Result<Vec<Arc<GraphProjectionHandle>>, GraphRegistryError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| GraphRegistryError::ResourceUnavailable)?;
        let mut removed = Vec::new();
        state.records.retain(|record| {
            if record.owner == owner {
                removed.push(Arc::clone(&record.handle));
                false
            } else {
                true
            }
        });
        Ok(removed)
    }

    pub(crate) fn clear_sessions(&self, owners: &[SessionIdentity]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state
            .records
            .retain(|record| !owners.contains(&record.owner));
    }

    pub(crate) fn reap(&self, now: Instant) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        reap_expired(&mut state, now);
    }

    pub(crate) fn clear(&self) -> Vec<Arc<GraphProjectionHandle>> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        state
            .records
            .drain(..)
            .map(|record| record.handle)
            .collect()
    }
}

fn reap_expired(state: &mut RegistryState, now: Instant) {
    state.records.retain(|record| {
        now.saturating_duration_since(record.last_seen) < PROJECTION_IDLE_TTL
            && now.saturating_duration_since(record.created_at) < PROJECTION_ABSOLUTE_TTL
    });
}

fn random_token() -> Result<(String, [u8; 32]), GraphRegistryError> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| GraphRegistryError::ResourceUnavailable)?;
    let encoded = BASE64URL_NOPAD.encode(&bytes);
    debug_assert_eq!(encoded.len(), TOKEN_LENGTH);
    Ok((encoded, sha2::Sha256::digest(bytes).into()))
}

fn decode_token_digest(token: &str) -> Result<[u8; 32], GraphRegistryError> {
    if token.len() != TOKEN_LENGTH {
        return Err(GraphRegistryError::Invalid);
    }
    let bytes = BASE64URL_NOPAD
        .decode(token.as_bytes())
        .map_err(|_| GraphRegistryError::Invalid)?;
    if bytes.len() != TOKEN_BYTES {
        return Err(GraphRegistryError::Invalid);
    }
    Ok(sha2::Sha256::digest(bytes).into())
}

#[cfg(test)]
mod tests {
    use rootlight_client::{
        AnalysisTier, ContinuationAvailability, CoverageStatus, GenerationId,
        GraphProjectionEffectiveBudget, GraphProjectionPage, QueryContext, QueryFreshness,
        QueryUsage, RepositoryId, ResultCompleteness, ResultCompletenessState,
    };

    use super::*;

    fn page(identifier: u8) -> GraphProjectionPage {
        GraphProjectionPage {
            projection: GraphProjectionId::from_bytes([identifier; 16]),
            context: QueryContext {
                repository: RepositoryId::from_bytes([1; 16]),
                generation: GenerationId::from_bytes([2; 20]),
                parent_generation: None,
                active_generation: true,
                structural_freshness: QueryFreshness::Current,
                semantic_freshness: QueryFreshness::Current,
                tier: AnalysisTier::TierB,
                coverage_status: CoverageStatus::Complete,
                skipped_inputs: 0,
                usage: QueryUsage {
                    rows: 0,
                    edges: 0,
                    results: 0,
                    source_bytes: 0,
                    json_bytes: 0,
                    estimated_tokens: 0,
                    token_accounting: None,
                    memory_bytes: None,
                    elapsed_micros: 0,
                },
            },
            nodes: Vec::new(),
            edges: Vec::new(),
            completeness: ResultCompleteness {
                state: ResultCompletenessState::Complete,
                limiting_resources: Vec::new(),
                continuation: ContinuationAvailability::NotApplicable,
                guidance: Vec::new(),
            },
            effective_budget: GraphProjectionEffectiveBudget {
                page_nodes: 1,
                page_edges: 1,
                aggregate_nodes: 1,
                aggregate_edges: 1,
            },
            returned_nodes_cumulative: 0,
            returned_edges_cumulative: 0,
            total_matching_nodes: 0,
            total_matching_edges: 0,
            total_known_nodes: Some(0),
            total_known_edges: Some(0),
            edges_omitted_for_unavailable_endpoints: 0,
            skipped_for_coverage: 0,
            continuation: None,
        }
    }

    fn identity(value: u8) -> SessionIdentity {
        SessionIdentity::from_test_bytes([value; 32])
    }

    #[test]
    fn projection_handles_are_session_bound_and_expire_absolutely() {
        let registry = GraphRegistry::new();
        let now = Instant::now();
        let issued = registry
            .issue(identity(1), &page(1), now)
            .expect("projection handle issues");

        assert!(registry.claim(identity(2), &issued.token, now).is_err());
        assert!(registry.claim(identity(1), &issued.token, now).is_ok());
        assert!(
            registry
                .claim(identity(1), &issued.token, now + PROJECTION_ABSOLUTE_TTL)
                .is_err()
        );
    }

    #[test]
    fn projection_capacity_and_exact_session_cleanup_are_bounded() {
        let registry = GraphRegistry::new();
        let now = Instant::now();
        for identifier in 0..MAX_PROJECTIONS_PER_SESSION {
            registry
                .issue(
                    identity(3),
                    &page(u8::try_from(identifier).expect("identifier fits")),
                    now,
                )
                .expect("session projection fits");
        }
        assert_eq!(
            registry.issue(identity(3), &page(10), now).err(),
            Some(GraphRegistryError::LimitReached)
        );
        let removed = registry
            .clear_session(identity(3))
            .expect("session cleanup succeeds");
        assert_eq!(removed.len(), MAX_PROJECTIONS_PER_SESSION);
        assert!(registry.issue(identity(3), &page(11), now).is_ok());
    }

    #[tokio::test]
    async fn exhausted_projection_never_reuses_a_cursor() {
        let handle = GraphProjectionHandle::new(&page(4));
        assert_eq!(
            handle.begin_next().await.err(),
            Some(GraphRegistryError::Exhausted)
        );
    }
}
