//! Short-lived browser capabilities for immutable daemon source references.
//!
//! The browser receives only random aliases; repository paths, byte ranges,
//! and content hashes remain inside the authenticated web process.

use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};

use data_encoding::BASE64URL_NOPAD;
use rootlight_client::{GenerationId, RepositoryId, SourceReference};
use sha2::Digest as _;
use subtle::ConstantTimeEq as _;

use crate::session::SessionIdentity;

const TOKEN_BYTES: usize = 32;
const TOKEN_LENGTH: usize = 43;
const CAPABILITY_TTL: Duration = Duration::from_secs(60);
const MAX_CAPABILITIES_PER_SESSION: usize = 128;
const MAX_CAPABILITIES_GLOBAL: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SourceRegistryError {
    #[error("source capability is invalid")]
    Invalid,
    #[error("source capability capacity is unavailable")]
    LimitReached,
    #[error("source capability registry is unavailable")]
    ResourceUnavailable,
}

pub(crate) struct IssuedSourceCapability {
    pub(crate) token: String,
    pub(crate) expires_in_seconds: u64,
}

pub(crate) struct SourceCapabilityRegistry {
    state: Mutex<VecDeque<SourceCapabilityRecord>>,
}

struct SourceCapabilityRecord {
    owner: SessionIdentity,
    token_digest: [u8; 32],
    created_at: Instant,
    reference: SourceReference,
}

impl SourceCapabilityRegistry {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(VecDeque::new()),
        }
    }

    pub(crate) fn issue_many(
        &self,
        owner: SessionIdentity,
        references: &[SourceReference],
        now: Instant,
    ) -> Result<Vec<IssuedSourceCapability>, SourceRegistryError> {
        if references.is_empty() {
            return Ok(Vec::new());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| SourceRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        let owner_count = state.iter().filter(|record| record.owner == owner).count();
        if owner_count.saturating_add(references.len()) > MAX_CAPABILITIES_PER_SESSION
            || state.len().saturating_add(references.len()) > MAX_CAPABILITIES_GLOBAL
        {
            return Err(SourceRegistryError::LimitReached);
        }

        let mut issued = Vec::new();
        issued
            .try_reserve_exact(references.len())
            .map_err(|_| SourceRegistryError::ResourceUnavailable)?;
        let mut records = Vec::new();
        records
            .try_reserve_exact(references.len())
            .map_err(|_| SourceRegistryError::ResourceUnavailable)?;
        for reference in references {
            let (token, token_digest) = random_token()?;
            issued.push(IssuedSourceCapability {
                token,
                expires_in_seconds: CAPABILITY_TTL.as_secs(),
            });
            records.push(SourceCapabilityRecord {
                owner,
                token_digest,
                created_at: now,
                reference: reference.clone(),
            });
        }
        state.extend(records);
        Ok(issued)
    }

    pub(crate) fn take(
        &self,
        owner: SessionIdentity,
        token: &str,
        repository: RepositoryId,
        generation: GenerationId,
        now: Instant,
    ) -> Result<SourceReference, SourceRegistryError> {
        let digest = decode_token_digest(token)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| SourceRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        let index = state
            .iter()
            .position(|record| {
                record.owner == owner && bool::from(record.token_digest.ct_eq(digest.as_slice()))
            })
            .ok_or(SourceRegistryError::Invalid)?;
        // An owner-authenticated attempt consumes the alias even when its
        // route correlation is wrong, preventing iterative capability probing.
        let record = state
            .remove(index)
            .ok_or(SourceRegistryError::ResourceUnavailable)?;
        if record.reference.repository() != repository
            || record.reference.generation() != generation
        {
            return Err(SourceRegistryError::Invalid);
        }
        Ok(record.reference)
    }

    pub(crate) fn clear_session(&self, owner: SessionIdentity) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.retain(|record| record.owner != owner);
    }

    pub(crate) fn clear_sessions(&self, owners: &[SessionIdentity]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.retain(|record| !owners.contains(&record.owner));
    }

    pub(crate) fn reap(&self, now: Instant) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        reap_expired(&mut state, now);
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.clear();
        }
    }
}

fn reap_expired(state: &mut VecDeque<SourceCapabilityRecord>, now: Instant) {
    state.retain(|record| now.saturating_duration_since(record.created_at) < CAPABILITY_TTL);
}

fn random_token() -> Result<(String, [u8; 32]), SourceRegistryError> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| SourceRegistryError::ResourceUnavailable)?;
    let encoded = BASE64URL_NOPAD.encode(&bytes);
    debug_assert_eq!(encoded.len(), TOKEN_LENGTH);
    Ok((encoded, sha2::Sha256::digest(bytes).into()))
}

fn decode_token_digest(token: &str) -> Result<[u8; 32], SourceRegistryError> {
    if token.len() != TOKEN_LENGTH {
        return Err(SourceRegistryError::Invalid);
    }
    let bytes = BASE64URL_NOPAD
        .decode(token.as_bytes())
        .map_err(|_| SourceRegistryError::Invalid)?;
    if bytes.len() != TOKEN_BYTES {
        return Err(SourceRegistryError::Invalid);
    }
    Ok(sha2::Sha256::digest(bytes).into())
}

#[cfg(test)]
mod tests {
    use rootlight_client::{ContentHash, FileId};

    use super::*;

    fn identity(value: u8) -> SessionIdentity {
        SessionIdentity::from_test_bytes([value; 32])
    }

    fn reference(repository: u8, generation: u8, file: u8) -> SourceReference {
        SourceReference::new(
            RepositoryId::from_bytes([repository; 16]),
            GenerationId::from_bytes([generation; 20]),
            FileId::from_bytes([file; 20]),
            10..20,
            ContentHash::from_bytes([file; 32]),
            Some(2..=3),
        )
        .expect("source fixture is valid")
    }

    #[test]
    fn capabilities_are_single_use_owner_bound_and_expire() {
        let registry = SourceCapabilityRegistry::new();
        let now = Instant::now();
        let source = reference(1, 2, 3);
        let issued = registry
            .issue_many(identity(4), std::slice::from_ref(&source), now)
            .expect("source capability issues")
            .remove(0);

        assert!(
            registry
                .take(
                    identity(5),
                    &issued.token,
                    source.repository(),
                    source.generation(),
                    now,
                )
                .is_err()
        );
        assert_eq!(
            registry
                .take(
                    identity(4),
                    &issued.token,
                    source.repository(),
                    source.generation(),
                    now,
                )
                .expect("owner consumes source capability"),
            source
        );
        assert!(
            registry
                .take(
                    identity(4),
                    &issued.token,
                    source.repository(),
                    source.generation(),
                    now,
                )
                .is_err()
        );

        let issued = registry
            .issue_many(identity(4), std::slice::from_ref(&source), now)
            .expect("replacement source capability issues")
            .remove(0);
        assert!(
            registry
                .take(
                    identity(4),
                    &issued.token,
                    source.repository(),
                    source.generation(),
                    now + CAPABILITY_TTL,
                )
                .is_err()
        );
    }

    #[test]
    fn correlation_mismatch_consumes_only_the_owner_capability() {
        let registry = SourceCapabilityRegistry::new();
        let now = Instant::now();
        let source = reference(1, 2, 3);
        let issued = registry
            .issue_many(identity(4), std::slice::from_ref(&source), now)
            .expect("source capability issues")
            .remove(0);

        assert!(
            registry
                .take(
                    identity(4),
                    &issued.token,
                    RepositoryId::from_bytes([9; 16]),
                    source.generation(),
                    now,
                )
                .is_err()
        );
        assert!(
            registry
                .take(
                    identity(4),
                    &issued.token,
                    source.repository(),
                    source.generation(),
                    now,
                )
                .is_err()
        );
    }

    #[test]
    fn session_and_shutdown_cleanup_are_exact() {
        let registry = SourceCapabilityRegistry::new();
        let now = Instant::now();
        let first = reference(1, 2, 3);
        let second = reference(1, 2, 4);
        let first_token = registry
            .issue_many(identity(5), std::slice::from_ref(&first), now)
            .expect("first capability issues")
            .remove(0)
            .token;
        let second_token = registry
            .issue_many(identity(6), std::slice::from_ref(&second), now)
            .expect("second capability issues")
            .remove(0)
            .token;

        registry.clear_session(identity(5));
        assert!(
            registry
                .take(
                    identity(5),
                    &first_token,
                    first.repository(),
                    first.generation(),
                    now,
                )
                .is_err()
        );
        assert!(
            registry
                .take(
                    identity(6),
                    &second_token,
                    second.repository(),
                    second.generation(),
                    now,
                )
                .is_ok()
        );

        let token = registry
            .issue_many(identity(6), std::slice::from_ref(&second), now)
            .expect("third capability issues")
            .remove(0)
            .token;
        registry.clear();
        assert!(
            registry
                .take(
                    identity(6),
                    &token,
                    second.repository(),
                    second.generation(),
                    now,
                )
                .is_err()
        );
    }

    #[test]
    fn expired_session_batch_cleanup_is_owner_scoped() {
        let registry = SourceCapabilityRegistry::new();
        let now = Instant::now();
        let first = reference(1, 2, 3);
        let second = reference(1, 2, 4);
        let first_token = registry
            .issue_many(identity(5), std::slice::from_ref(&first), now)
            .expect("first capability issues")
            .remove(0)
            .token;
        let second_token = registry
            .issue_many(identity(6), std::slice::from_ref(&second), now)
            .expect("second capability issues")
            .remove(0)
            .token;

        registry.clear_sessions(&[identity(5)]);
        assert!(
            registry
                .take(
                    identity(5),
                    &first_token,
                    first.repository(),
                    first.generation(),
                    now,
                )
                .is_err()
        );
        assert!(
            registry
                .take(
                    identity(6),
                    &second_token,
                    second.repository(),
                    second.generation(),
                    now,
                )
                .is_ok()
        );
    }
}
