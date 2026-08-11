//! Bounded one-time bootstrap and browser-session credential lifecycle.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use data_encoding::BASE64URL_NOPAD;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq;

use crate::error::WebError;

pub(crate) const SESSION_COOKIE_NAME: &str = "rootlight_session";
pub(crate) const CSRF_HEADER_NAME: &str = "x-rootlight-csrf";
const SECRET_BYTES: usize = 32;
const ENCODED_SECRET_BYTES: usize = 43;
const BOOTSTRAP_TTL: Duration = Duration::from_secs(10 * 60);
const SESSION_IDLE_TTL: Duration = Duration::from_secs(30 * 60);
pub(crate) const SESSION_ABSOLUTE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_OUTSTANDING_BOOTSTRAPS: usize = 8;
const MAX_SESSIONS: usize = 32;

pub(crate) struct BootstrapSecret {
    encoded: String,
}

impl BootstrapSecret {
    pub(crate) fn encoded(&self) -> &str {
        &self.encoded
    }
}

/// Returns the browser-session idle timeout exposed by the API.
#[must_use]
pub(crate) const fn idle_ttl_seconds() -> u64 {
    SESSION_IDLE_TTL.as_secs()
}

pub(crate) struct SessionCredentials {
    pub(crate) cookie_value: String,
    pub(crate) csrf_token: String,
    pub(crate) idle_ttl_seconds: u64,
}

#[derive(Clone)]
pub(crate) struct AuthenticatedSession {
    key: [u8; 32],
    csrf: [u8; SECRET_BYTES],
}

impl AuthenticatedSession {
    pub(crate) const fn identity(&self) -> SessionIdentity {
        SessionIdentity(self.key)
    }

    pub(crate) fn csrf_token(&self) -> String {
        BASE64URL_NOPAD.encode(&self.csrf)
    }

    pub(crate) fn validate_csrf(&self, encoded: &str) -> bool {
        decode_secret(encoded).is_some_and(|candidate| bool::from(self.csrf.ct_eq(&candidate)))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionIdentity([u8; 32]);

#[cfg(test)]
impl SessionIdentity {
    pub(crate) const fn from_test_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

pub(crate) struct SessionRegistry {
    state: Mutex<RegistryState>,
}

struct RegistryState {
    bootstraps: Vec<BootstrapRecord>,
    sessions: Vec<SessionRecord>,
}

struct BootstrapRecord {
    digest: [u8; 32],
    expires_at: Instant,
}

struct SessionRecord {
    key: [u8; 32],
    csrf: [u8; SECRET_BYTES],
    created_at: Instant,
    last_seen: Instant,
}

impl SessionRegistry {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                bootstraps: Vec::new(),
                sessions: Vec::new(),
            }),
        }
    }

    pub(crate) fn issue_bootstrap(&self, now: Instant) -> Result<BootstrapSecret, WebError> {
        let secret = random_secret()?;
        let digest = secret_digest(&secret);
        let expires_at = now
            .checked_add(BOOTSTRAP_TTL)
            .ok_or(WebError::RandomUnavailable)?;
        let mut state = self.state.lock().map_err(|_| WebError::TaskFailed)?;
        reap_expired(&mut state, now);
        if state.bootstraps.len() >= MAX_OUTSTANDING_BOOTSTRAPS {
            return Err(WebError::RandomUnavailable);
        }
        state
            .bootstraps
            .push(BootstrapRecord { digest, expires_at });
        Ok(BootstrapSecret {
            encoded: BASE64URL_NOPAD.encode(&secret),
        })
    }

    pub(crate) fn consume_bootstrap(
        &self,
        encoded: &str,
        now: Instant,
    ) -> Option<SessionCredentials> {
        let secret = decode_secret(encoded)?;
        let digest = secret_digest(&secret);
        let mut state = self.state.lock().ok()?;
        reap_expired(&mut state, now);
        let index = constant_time_bootstrap_index(&state.bootstraps, &digest)?;
        state.bootstraps.remove(index);
        create_session(&mut state, now).ok()
    }

    #[cfg(test)]
    pub(crate) fn issue_session(&self, now: Instant) -> Result<SessionCredentials, WebError> {
        let mut state = self.state.lock().map_err(|_| WebError::TaskFailed)?;
        reap_expired(&mut state, now);
        create_session(&mut state, now)
    }

    pub(crate) fn authenticate(
        &self,
        cookie_value: &str,
        now: Instant,
    ) -> Option<AuthenticatedSession> {
        let session_id = decode_secret(cookie_value)?;
        let key = secret_digest(&session_id);
        let mut state = self.state.lock().ok()?;
        reap_expired(&mut state, now);
        let index = constant_time_session_index(&state.sessions, &key)?;
        let session = state.sessions.get_mut(index)?;
        session.last_seen = now;
        Some(AuthenticatedSession {
            key: session.key,
            csrf: session.csrf,
        })
    }

    pub(crate) fn expire(&self, now: Instant) -> Vec<SessionIdentity> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        reap_expired(&mut state, now)
    }

    pub(crate) fn logout(&self, session: &AuthenticatedSession) {
        if let Ok(mut state) = self.state.lock()
            && let Some(index) = constant_time_session_index(&state.sessions, &session.key)
        {
            state.sessions.remove(index);
        }
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.bootstraps.clear();
            state.sessions.clear();
        }
    }
}

fn constant_time_bootstrap_index(
    records: &[BootstrapRecord],
    candidate: &[u8; 32],
) -> Option<usize> {
    records
        .iter()
        .enumerate()
        .fold(None, |matched, (index, record)| {
            if bool::from(record.digest.ct_eq(candidate)) {
                Some(index)
            } else {
                matched
            }
        })
}

fn create_session(state: &mut RegistryState, now: Instant) -> Result<SessionCredentials, WebError> {
    if state.sessions.len() >= MAX_SESSIONS {
        return Err(WebError::RandomUnavailable);
    }
    let session_id = random_secret()?;
    let csrf = random_secret()?;
    let key = secret_digest(&session_id);
    state.sessions.push(SessionRecord {
        key,
        csrf,
        created_at: now,
        last_seen: now,
    });
    Ok(SessionCredentials {
        cookie_value: BASE64URL_NOPAD.encode(&session_id),
        csrf_token: BASE64URL_NOPAD.encode(&csrf),
        idle_ttl_seconds: SESSION_IDLE_TTL.as_secs(),
    })
}

fn random_secret() -> Result<[u8; SECRET_BYTES], WebError> {
    let mut secret = [0_u8; SECRET_BYTES];
    getrandom::fill(&mut secret).map_err(|_| WebError::RandomUnavailable)?;
    Ok(secret)
}

fn decode_secret(encoded: &str) -> Option<[u8; SECRET_BYTES]> {
    if encoded.len() != ENCODED_SECRET_BYTES {
        return None;
    }
    let decoded = BASE64URL_NOPAD.decode(encoded.as_bytes()).ok()?;
    decoded.try_into().ok()
}

fn secret_digest(secret: &[u8; SECRET_BYTES]) -> [u8; 32] {
    Sha256::digest(secret).into()
}

fn constant_time_session_index(records: &[SessionRecord], candidate: &[u8; 32]) -> Option<usize> {
    records
        .iter()
        .enumerate()
        .fold(None, |matched, (index, record)| {
            if bool::from(record.key.ct_eq(candidate)) {
                Some(index)
            } else {
                matched
            }
        })
}

fn reap_expired(state: &mut RegistryState, now: Instant) -> Vec<SessionIdentity> {
    state.bootstraps.retain(|record| record.expires_at > now);
    let mut expired = Vec::new();
    state.sessions.retain(|record| {
        let idle_valid = now
            .checked_duration_since(record.last_seen)
            .is_some_and(|idle| idle < SESSION_IDLE_TTL);
        let lifetime_valid = now
            .checked_duration_since(record.created_at)
            .is_some_and(|lifetime| lifetime < SESSION_ABSOLUTE_TTL);
        let valid = idle_valid && lifetime_valid;
        if !valid {
            expired.push(SessionIdentity(record.key));
        }
        valid
    });
    expired
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_is_single_use_and_rejects_invalid_credentials() {
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let bootstrap = registry.issue_bootstrap(now).expect("bootstrap issues");

        assert!(registry.consume_bootstrap("too-short", now).is_none());
        assert!(
            registry
                .consume_bootstrap(&"b".repeat(ENCODED_SECRET_BYTES), now)
                .is_none()
        );
        assert!(
            registry
                .consume_bootstrap(bootstrap.encoded(), now)
                .is_some()
        );
        assert!(
            registry
                .consume_bootstrap(bootstrap.encoded(), now)
                .is_none()
        );
    }

    #[test]
    fn bootstrap_expiry_and_capacity_fail_closed() {
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let mut bootstraps = Vec::new();
        for _ in 0..MAX_OUTSTANDING_BOOTSTRAPS {
            bootstraps.push(registry.issue_bootstrap(now).expect("bootstrap issues"));
        }
        assert!(registry.issue_bootstrap(now).is_err());

        let at_expiry = now
            .checked_add(BOOTSTRAP_TTL)
            .expect("test time is representable");
        assert!(
            registry
                .consume_bootstrap(bootstraps[0].encoded(), at_expiry)
                .is_none()
        );
        assert!(registry.issue_bootstrap(at_expiry).is_ok());
    }

    #[test]
    fn sessions_require_exact_credentials_and_csrf() {
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let credentials = registry
            .issue_session(now)
            .expect("browser session credentials issue");

        let session = registry
            .authenticate(&credentials.cookie_value, now)
            .expect("session authenticates");
        assert!(session.validate_csrf(&credentials.csrf_token));
        assert!(!session.validate_csrf("invalid"));
        registry.logout(&session);
        assert!(
            registry
                .authenticate(&credentials.cookie_value, now)
                .is_none()
        );
    }

    #[test]
    fn idle_and_absolute_expiry_fail_closed() {
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let credentials = registry
            .issue_session(now)
            .expect("browser session credentials issue");
        let after_idle_expiry = now
            .checked_add(SESSION_IDLE_TTL + Duration::from_nanos(1))
            .expect("test time is representable");
        assert!(
            registry
                .authenticate(&credentials.cookie_value, after_idle_expiry)
                .is_none()
        );

        let credentials = registry
            .issue_session(now)
            .expect("another browser session issues");
        let after_absolute_expiry = now
            .checked_add(SESSION_ABSOLUTE_TTL + Duration::from_nanos(1))
            .expect("test time is representable");
        assert!(
            registry
                .authenticate(&credentials.cookie_value, after_absolute_expiry)
                .is_none()
        );
    }
}
