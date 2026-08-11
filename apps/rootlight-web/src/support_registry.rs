//! Bounded single-use download receipts for source-free support archives.

use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};

use data_encoding::BASE64URL_NOPAD;
use sha2::Digest as _;
use subtle::ConstantTimeEq as _;

use crate::session::SessionIdentity;

const TOKEN_BYTES: usize = 32;
const TOKEN_LENGTH: usize = 43;
const ARTIFACT_TTL: Duration = Duration::from_secs(2 * 60);
const MAX_ARTIFACTS_GLOBAL: usize = 16;
const MAX_ARCHIVE_BYTES: usize = 768 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SupportRegistryError {
    #[error("support bundle receipt is invalid")]
    Invalid,
    #[error("support bundle capacity is unavailable")]
    LimitReached,
    #[error("support bundle archive is invalid")]
    ArchiveInvalid,
    #[error("support bundle registry is unavailable")]
    ResourceUnavailable,
}

pub(crate) struct SupportArtifact {
    pub(crate) archive: Vec<u8>,
    pub(crate) sha256: [u8; 32],
}

pub(crate) struct IssuedSupportArtifact {
    pub(crate) receipt: String,
    pub(crate) archive_bytes: u64,
    pub(crate) sha256: [u8; 32],
    pub(crate) expires_in_seconds: u64,
}

pub(crate) struct SupportRegistry {
    state: Mutex<RegistryState>,
}

#[must_use = "dropping the reservation releases its registry slot"]
pub(crate) struct SupportReservation<'a> {
    registry: &'a SupportRegistry,
    owner: SessionIdentity,
    reservation: ReservationIdentity,
}

struct RegistryState {
    next_reservation: u64,
    reservations: Vec<ReservationRecord>,
    artifacts: VecDeque<ArtifactRecord>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ReservationIdentity(u64);

struct ReservationRecord {
    identity: ReservationIdentity,
    owner: SessionIdentity,
}

struct ArtifactRecord {
    owner: SessionIdentity,
    receipt_digest: [u8; 32],
    created_at: Instant,
    artifact: SupportArtifact,
}

impl SupportRegistry {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                next_reservation: 1,
                reservations: Vec::new(),
                artifacts: VecDeque::new(),
            }),
        }
    }

    fn reserve(
        &self,
        owner: SessionIdentity,
        now: Instant,
    ) -> Result<ReservationIdentity, SupportRegistryError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SupportRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        if state
            .reservations
            .iter()
            .any(|reservation| reservation.owner == owner)
            || state.artifacts.iter().any(|record| record.owner == owner)
            || state.artifacts.len() + state.reservations.len() >= MAX_ARTIFACTS_GLOBAL
        {
            return Err(SupportRegistryError::LimitReached);
        }
        let identity = ReservationIdentity(state.next_reservation);
        state.next_reservation = state
            .next_reservation
            .checked_add(1)
            .ok_or(SupportRegistryError::ResourceUnavailable)?;
        state
            .reservations
            .push(ReservationRecord { identity, owner });
        Ok(identity)
    }

    pub(crate) fn reserve_guard(
        &self,
        owner: SessionIdentity,
        now: Instant,
    ) -> Result<SupportReservation<'_>, SupportRegistryError> {
        let reservation = self.reserve(owner, now)?;
        Ok(SupportReservation {
            registry: self,
            owner,
            reservation,
        })
    }

    #[cfg(test)]
    fn issue_reserved(
        &self,
        owner: SessionIdentity,
        archive: Vec<u8>,
        sha256: [u8; 32],
        now: Instant,
    ) -> Result<IssuedSupportArtifact, SupportRegistryError> {
        self.issue_reservation(owner, None, archive, sha256, now)
    }

    fn issue_reservation(
        &self,
        owner: SessionIdentity,
        reservation: Option<ReservationIdentity>,
        archive: Vec<u8>,
        sha256: [u8; 32],
        now: Instant,
    ) -> Result<IssuedSupportArtifact, SupportRegistryError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SupportRegistryError::ResourceUnavailable)?;
        let Some(reservation) = state.reservations.iter().position(|candidate| {
            candidate.owner == owner
                && reservation.is_none_or(|expected| candidate.identity == expected)
        }) else {
            return Err(SupportRegistryError::Invalid);
        };
        state.reservations.remove(reservation);
        if archive.is_empty()
            || archive.len() > MAX_ARCHIVE_BYTES
            || !bool::from(sha256.ct_eq(sha2::Sha256::digest(&archive).as_slice()))
        {
            return Err(SupportRegistryError::ArchiveInvalid);
        }
        let (receipt, receipt_digest) = random_receipt()?;
        let archive_bytes =
            u64::try_from(archive.len()).map_err(|_| SupportRegistryError::ArchiveInvalid)?;
        state.artifacts.push_back(ArtifactRecord {
            owner,
            receipt_digest,
            created_at: now,
            artifact: SupportArtifact { archive, sha256 },
        });
        Ok(IssuedSupportArtifact {
            receipt,
            archive_bytes,
            sha256,
            expires_in_seconds: ARTIFACT_TTL.as_secs(),
        })
    }

    fn abort_reservation(&self, reservation: ReservationIdentity) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state
            .reservations
            .retain(|candidate| candidate.identity != reservation);
    }

    pub(crate) fn take(
        &self,
        owner: SessionIdentity,
        receipt: &str,
        now: Instant,
    ) -> Result<SupportArtifact, SupportRegistryError> {
        let digest = decode_receipt_digest(receipt)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| SupportRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        let index = state
            .artifacts
            .iter()
            .position(|record| {
                record.owner == owner && bool::from(record.receipt_digest.ct_eq(digest.as_slice()))
            })
            .ok_or(SupportRegistryError::Invalid)?;
        Ok(state
            .artifacts
            .remove(index)
            .ok_or(SupportRegistryError::ResourceUnavailable)?
            .artifact)
    }

    pub(crate) fn clear_session(&self, owner: SessionIdentity) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state
            .reservations
            .retain(|candidate| candidate.owner != owner);
        state.artifacts.retain(|record| record.owner != owner);
    }

    pub(crate) fn clear_sessions(&self, owners: &[SessionIdentity]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state
            .reservations
            .retain(|candidate| !owners.contains(&candidate.owner));
        state
            .artifacts
            .retain(|record| !owners.contains(&record.owner));
    }

    pub(crate) fn reap(&self, now: Instant) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        reap_expired(&mut state, now);
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.reservations.clear();
            state.artifacts.clear();
        }
    }
}

impl SupportReservation<'_> {
    pub(crate) fn issue(
        self,
        archive: Vec<u8>,
        sha256: [u8; 32],
        now: Instant,
    ) -> Result<IssuedSupportArtifact, SupportRegistryError> {
        self.registry
            .issue_reservation(self.owner, Some(self.reservation), archive, sha256, now)
    }
}

impl Drop for SupportReservation<'_> {
    fn drop(&mut self) {
        self.registry.abort_reservation(self.reservation);
    }
}

fn reap_expired(state: &mut RegistryState, now: Instant) {
    state
        .artifacts
        .retain(|record| now.saturating_duration_since(record.created_at) < ARTIFACT_TTL);
}

fn random_receipt() -> Result<(String, [u8; 32]), SupportRegistryError> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| SupportRegistryError::ResourceUnavailable)?;
    let encoded = BASE64URL_NOPAD.encode(&bytes);
    debug_assert_eq!(encoded.len(), TOKEN_LENGTH);
    Ok((encoded, sha2::Sha256::digest(bytes).into()))
}

fn decode_receipt_digest(receipt: &str) -> Result<[u8; 32], SupportRegistryError> {
    if receipt.len() != TOKEN_LENGTH {
        return Err(SupportRegistryError::Invalid);
    }
    let bytes = BASE64URL_NOPAD
        .decode(receipt.as_bytes())
        .map_err(|_| SupportRegistryError::Invalid)?;
    if bytes.len() != TOKEN_BYTES {
        return Err(SupportRegistryError::Invalid);
    }
    Ok(sha2::Sha256::digest(bytes).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(value: u8) -> SessionIdentity {
        SessionIdentity::from_test_bytes([value; 32])
    }

    fn archive() -> (Vec<u8>, [u8; 32]) {
        let archive = b"PK\x03\x04source-free-support".to_vec();
        let digest = sha2::Sha256::digest(&archive).into();
        (archive, digest)
    }

    #[test]
    fn support_receipts_are_single_use_session_bound_and_expiring() {
        let registry = SupportRegistry::new();
        let now = Instant::now();
        registry
            .reserve(identity(1), now)
            .expect("support slot reserves");
        let (archive_bytes, digest) = archive();
        let issued = registry
            .issue_reserved(identity(1), archive_bytes.clone(), digest, now)
            .expect("support artifact issues");

        assert!(registry.take(identity(2), &issued.receipt, now).is_err());
        assert_eq!(
            registry
                .take(identity(1), &issued.receipt, now)
                .expect("owner consumes receipt")
                .archive,
            archive_bytes
        );
        assert!(registry.take(identity(1), &issued.receipt, now).is_err());

        registry
            .reserve(identity(1), now)
            .expect("slot reopens after consumption");
        let (archive_bytes, digest) = archive();
        let issued = registry
            .issue_reserved(identity(1), archive_bytes, digest, now)
            .expect("second artifact issues");
        assert!(
            registry
                .take(identity(1), &issued.receipt, now + ARTIFACT_TTL)
                .is_err()
        );
    }

    #[test]
    fn support_admission_is_one_per_session_and_digest_checked() {
        let registry = SupportRegistry::new();
        let now = Instant::now();
        registry
            .reserve(identity(3), now)
            .expect("support slot reserves");
        assert_eq!(
            registry.reserve(identity(3), now).err(),
            Some(SupportRegistryError::LimitReached)
        );
        let (archive, _) = archive();
        assert_eq!(
            registry
                .issue_reserved(identity(3), archive, [0; 32], now)
                .err(),
            Some(SupportRegistryError::ArchiveInvalid)
        );
        assert!(registry.reserve(identity(3), now).is_ok());
    }

    #[test]
    fn dropping_pending_reservation_releases_its_slot() {
        let registry = SupportRegistry::new();
        let now = Instant::now();
        let reservation = registry
            .reserve_guard(identity(4), now)
            .expect("support slot reserves");

        assert_eq!(
            registry.reserve(identity(4), now).err(),
            Some(SupportRegistryError::LimitReached)
        );
        drop(reservation);
        assert!(registry.reserve(identity(4), now).is_ok());
    }

    #[test]
    fn failed_issue_consumes_only_the_guarded_reservation() {
        let registry = SupportRegistry::new();
        let now = Instant::now();
        let reservation = registry
            .reserve_guard(identity(5), now)
            .expect("support slot reserves");
        let (archive, _) = archive();

        assert_eq!(
            reservation.issue(archive, [0; 32], now).err(),
            Some(SupportRegistryError::ArchiveInvalid)
        );
        assert!(registry.reserve(identity(5), now).is_ok());
    }
}
