//! Session-owned filesystem capabilities for the browser-facing BFF.
//!
//! Opaque keys retain VFS directory handles and immutable browse snapshots;
//! browser-provided strings never become filesystem authorization.

use std::{
    fmt,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use data_encoding::BASE64URL_NOPAD;
use rootlight_cancel::Cancellation;
use rootlight_vfs::{BrowseDirectory, BrowseDirectorySnapshot, BrowseError};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

use crate::session::SessionIdentity;

const TOKEN_BYTES: usize = 32;
const BROWSE_IDLE_TTL: Duration = Duration::from_secs(5 * 60);
const BROWSE_ABSOLUTE_TTL: Duration = Duration::from_secs(15 * 60);
const ROOT_CAPABILITY_IDLE_TTL: Duration = Duration::from_secs(2 * 60);
const ROOT_CAPABILITY_ABSOLUTE_TTL: Duration = Duration::from_secs(2 * 60);
const CURSOR_IDLE_TTL: Duration = Duration::from_secs(2 * 60);
const CURSOR_ABSOLUTE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CAPABILITIES_PER_SESSION: usize = 64;
const MAX_CAPABILITIES_GLOBAL: usize = 256;
const MAX_CURSORS_PER_SESSION: usize = 128;
const MAX_CURSORS_GLOBAL: usize = 512;
pub(crate) const MAX_BROWSE_DEPTH: usize = 32;
pub(crate) const ROOT_CAPABILITY_TTL_SECONDS: u64 = ROOT_CAPABILITY_ABSOLUTE_TTL.as_secs();

pub(crate) struct FilesystemRegistry {
    state: Mutex<RegistryState>,
}

struct RegistryState {
    capabilities: Vec<CapabilityRecord>,
    cursors: Vec<CursorRecord>,
}

struct CapabilityRecord {
    owner: SessionIdentity,
    created_at: Instant,
    last_seen: Instant,
    value: Capability,
}

enum Capability {
    Browse(Arc<BrowseNode>),
    Root {
        token: OpaqueToken,
        admission: Arc<RootAdmission>,
        idempotency_digest: Option<[u8; 32]>,
    },
}

pub(crate) struct BrowseNode {
    owner: SessionIdentity,
    token: OpaqueToken,
    created_at: Instant,
    directory: Arc<BrowseDirectory>,
    snapshot: Mutex<Option<Arc<BrowseDirectorySnapshot>>>,
    label: String,
    parent: Option<Arc<Self>>,
    depth: usize,
}

impl BrowseNode {
    pub(crate) fn token(&self) -> &str {
        &self.token.encoded
    }

    pub(crate) fn token_digest(&self) -> [u8; 32] {
        self.token.digest
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn parent(&self) -> Option<Arc<Self>> {
        self.parent.as_ref().map(Arc::clone)
    }

    pub(crate) const fn depth(&self) -> usize {
        self.depth
    }

    pub(crate) fn directory(&self) -> &BrowseDirectory {
        &self.directory
    }

    pub(crate) fn ancestors(&self) -> Vec<Arc<Self>> {
        let mut ancestors = Vec::with_capacity(self.depth.saturating_add(1));
        let mut current = self.parent();
        while let Some(node) = current {
            current = node.parent();
            ancestors.push(node);
        }
        ancestors.reverse();
        ancestors
    }

    pub(crate) fn snapshot(
        &self,
        cancellation: &Cancellation,
    ) -> Result<Arc<BrowseDirectorySnapshot>, FilesystemRegistryError> {
        if let Some(snapshot) = self
            .snapshot
            .lock()
            .map_err(|_| FilesystemRegistryError::ResourceUnavailable)?
            .as_ref()
        {
            return Ok(Arc::clone(snapshot));
        }

        let captured = Arc::new(
            self.directory
                .snapshot(cancellation)
                .map_err(FilesystemRegistryError::from_browse)?,
        );
        let mut stored = self
            .snapshot
            .lock()
            .map_err(|_| FilesystemRegistryError::ResourceUnavailable)?;
        if let Some(snapshot) = stored.as_ref() {
            return Ok(Arc::clone(snapshot));
        }
        *stored = Some(Arc::clone(&captured));
        Ok(captured)
    }
}

impl fmt::Debug for BrowseNode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowseNode")
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

pub(crate) struct RootAdmission {
    directory: BrowseDirectory,
    display_label: String,
}

impl RootAdmission {
    pub(crate) fn new(directory: BrowseDirectory, display_label: String) -> Self {
        Self {
            directory,
            display_label,
        }
    }

    pub(crate) fn local_path(&self) -> &Path {
        self.directory.local_path()
    }

    pub(crate) fn display_label(&self) -> &str {
        &self.display_label
    }
}

impl fmt::Debug for RootAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RootAdmission")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) struct IssuedBrowseCapability {
    pub(crate) token: String,
    pub(crate) node: Arc<BrowseNode>,
}

pub(crate) struct IssuedRootCapability {
    pub(crate) token: String,
}

struct CursorRecord {
    owner: SessionIdentity,
    token: OpaqueToken,
    browse_digest: [u8; 32],
    filter_digest: [u8; 32],
    offset: usize,
    created_at: Instant,
    last_seen: Instant,
}

struct OpaqueToken {
    encoded: String,
    digest: [u8; 32],
}

impl FilesystemRegistry {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                capabilities: Vec::new(),
                cursors: Vec::new(),
            }),
        }
    }

    pub(crate) fn issue_browse(
        &self,
        owner: SessionIdentity,
        directory: BrowseDirectory,
        snapshot: Option<Arc<BrowseDirectorySnapshot>>,
        label: String,
        parent: Option<Arc<BrowseNode>>,
        now: Instant,
    ) -> Result<IssuedBrowseCapability, FilesystemRegistryError> {
        let depth = parent
            .as_ref()
            .map_or(0, |parent| parent.depth.saturating_add(1));
        if depth > MAX_BROWSE_DEPTH {
            return Err(FilesystemRegistryError::LimitReached);
        }
        if parent.as_ref().is_some_and(|parent| parent.owner != owner) {
            return Err(FilesystemRegistryError::CapabilityInvalid);
        }

        let token = random_token()?;
        let node = Arc::new(BrowseNode {
            owner,
            token,
            created_at: now,
            directory: Arc::new(directory),
            snapshot: Mutex::new(snapshot),
            label,
            parent,
            depth,
        });
        let mut state = self
            .state
            .lock()
            .map_err(|_| FilesystemRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        admit_capability(&mut state, owner);
        state.capabilities.push(CapabilityRecord {
            owner,
            created_at: now,
            last_seen: now,
            value: Capability::Browse(Arc::clone(&node)),
        });
        Ok(IssuedBrowseCapability {
            token: node.token.encoded.clone(),
            node,
        })
    }

    pub(crate) fn resolve_browse(
        &self,
        owner: SessionIdentity,
        encoded: &str,
        now: Instant,
    ) -> Result<Arc<BrowseNode>, FilesystemRegistryError> {
        let digest = decode_token_digest(encoded)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| FilesystemRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        let record = state
            .capabilities
            .iter_mut()
            .find(|record| {
                record.owner == owner
                    && matches!(&record.value, Capability::Browse(node) if token_matches(&node.token.digest, &digest))
            })
            .ok_or(FilesystemRegistryError::CapabilityInvalid)?;
        record.last_seen = now;
        match &record.value {
            Capability::Browse(node) => Ok(Arc::clone(node)),
            Capability::Root { .. } => Err(FilesystemRegistryError::CapabilityInvalid),
        }
    }

    pub(crate) fn retain_browse(
        &self,
        owner: SessionIdentity,
        node: Arc<BrowseNode>,
        now: Instant,
    ) -> Result<IssuedBrowseCapability, FilesystemRegistryError> {
        if node.owner != owner || !absolute_valid(node.created_at, BROWSE_ABSOLUTE_TTL, now) {
            return Err(FilesystemRegistryError::CapabilityInvalid);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| FilesystemRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        if let Some(record) = state.capabilities.iter_mut().find(|record| {
            record.owner == owner
                && matches!(&record.value, Capability::Browse(existing) if Arc::ptr_eq(existing, &node))
        }) {
            record.last_seen = now;
        } else {
            admit_capability(&mut state, owner);
            state.capabilities.push(CapabilityRecord {
                owner,
                created_at: node.created_at,
                last_seen: now,
                value: Capability::Browse(Arc::clone(&node)),
            });
        }
        Ok(IssuedBrowseCapability {
            token: node.token.encoded.clone(),
            node,
        })
    }

    pub(crate) fn issue_root(
        &self,
        owner: SessionIdentity,
        admission: RootAdmission,
        now: Instant,
    ) -> Result<IssuedRootCapability, FilesystemRegistryError> {
        let token = random_token()?;
        let encoded = token.encoded.clone();
        let mut state = self
            .state
            .lock()
            .map_err(|_| FilesystemRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        admit_capability(&mut state, owner);
        state.capabilities.push(CapabilityRecord {
            owner,
            created_at: now,
            last_seen: now,
            value: Capability::Root {
                token,
                admission: Arc::new(admission),
                idempotency_digest: None,
            },
        });
        Ok(IssuedRootCapability { token: encoded })
    }

    pub(crate) fn bind_root(
        &self,
        owner: SessionIdentity,
        encoded: &str,
        idempotency_digest: [u8; 32],
        now: Instant,
    ) -> Result<Arc<RootAdmission>, FilesystemRegistryError> {
        let digest = decode_token_digest(encoded)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| FilesystemRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        let record = state
            .capabilities
            .iter_mut()
            .find(|record| {
                record.owner == owner
                    && matches!(&record.value, Capability::Root { token, .. } if token_matches(&token.digest, &digest))
            })
            .ok_or(FilesystemRegistryError::CapabilityInvalid)?;
        record.last_seen = now;
        match &mut record.value {
            Capability::Root {
                admission,
                idempotency_digest: bound_digest,
                ..
            } if bound_digest.is_none() || *bound_digest == Some(idempotency_digest) => {
                *bound_digest = Some(idempotency_digest);
                Ok(Arc::clone(admission))
            }
            Capability::Root { .. } => Err(FilesystemRegistryError::CapabilityInvalid),
            Capability::Browse(_) => Err(FilesystemRegistryError::CapabilityInvalid),
        }
    }

    pub(crate) fn issue_cursor(
        &self,
        owner: SessionIdentity,
        browse_digest: [u8; 32],
        filter_digest: [u8; 32],
        offset: usize,
        now: Instant,
    ) -> Result<String, FilesystemRegistryError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| FilesystemRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        if let Some(cursor) = state.cursors.iter_mut().find(|cursor| {
            cursor.owner == owner
                && cursor.browse_digest == browse_digest
                && cursor.filter_digest == filter_digest
                && cursor.offset == offset
        }) {
            cursor.last_seen = now;
            return Ok(cursor.token.encoded.clone());
        }

        admit_cursor(&mut state, owner);
        let token = random_token()?;
        let encoded = token.encoded.clone();
        state.cursors.push(CursorRecord {
            owner,
            token,
            browse_digest,
            filter_digest,
            offset,
            created_at: now,
            last_seen: now,
        });
        Ok(encoded)
    }

    pub(crate) fn resolve_cursor(
        &self,
        owner: SessionIdentity,
        encoded: &str,
        browse_digest: &[u8; 32],
        filter_digest: &[u8; 32],
        now: Instant,
    ) -> Result<usize, FilesystemRegistryError> {
        let digest = decode_token_digest(encoded)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| FilesystemRegistryError::ResourceUnavailable)?;
        reap_expired(&mut state, now);
        let cursor = state
            .cursors
            .iter_mut()
            .find(|cursor| {
                cursor.owner == owner
                    && token_matches(&cursor.token.digest, &digest)
                    && token_matches(&cursor.browse_digest, browse_digest)
                    && token_matches(&cursor.filter_digest, filter_digest)
            })
            .ok_or(FilesystemRegistryError::CapabilityInvalid)?;
        cursor.last_seen = now;
        Ok(cursor.offset)
    }

    pub(crate) fn clear_session(&self, owner: SessionIdentity) {
        if let Ok(mut state) = self.state.lock() {
            state.capabilities.retain(|record| record.owner != owner);
            state.cursors.retain(|record| record.owner != owner);
        }
    }

    pub(crate) fn clear_sessions(&self, owners: &[SessionIdentity]) {
        if let Ok(mut state) = self.state.lock() {
            state
                .capabilities
                .retain(|record| !owners.contains(&record.owner));
            state
                .cursors
                .retain(|record| !owners.contains(&record.owner));
        }
    }

    pub(crate) fn reap(&self, now: Instant) {
        if let Ok(mut state) = self.state.lock() {
            reap_expired(&mut state, now);
        }
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.capabilities.clear();
            state.cursors.clear();
        }
    }

    #[cfg(test)]
    fn capability_count(&self, owner: SessionIdentity) -> usize {
        self.state
            .lock()
            .map(|state| {
                state
                    .capabilities
                    .iter()
                    .filter(|record| record.owner == owner)
                    .count()
            })
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilesystemRegistryError {
    InvalidRequest,
    CapabilityInvalid,
    LimitReached,
    ResourceUnavailable,
}

impl FilesystemRegistryError {
    pub(crate) fn from_browse(error: BrowseError) -> Self {
        match error {
            BrowseError::InvalidRootPath
            | BrowseError::RootPathTooLong { .. }
            | BrowseError::InvalidChildName { .. }
            | BrowseError::InvalidEntryLimit { .. }
            | BrowseError::InvalidPageSize { .. }
            | BrowseError::InvalidPageOffset { .. } => Self::InvalidRequest,
            BrowseError::EntryLimitExceeded { .. } => Self::LimitReached,
            _ => Self::ResourceUnavailable,
        }
    }
}

fn random_token() -> Result<OpaqueToken, FilesystemRegistryError> {
    let mut secret = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut secret).map_err(|_| FilesystemRegistryError::ResourceUnavailable)?;
    Ok(OpaqueToken {
        encoded: BASE64URL_NOPAD.encode(&secret),
        digest: Sha256::digest(secret).into(),
    })
}

fn decode_token_digest(encoded: &str) -> Result<[u8; 32], FilesystemRegistryError> {
    let decoded = BASE64URL_NOPAD
        .decode(encoded.as_bytes())
        .map_err(|_| FilesystemRegistryError::CapabilityInvalid)?;
    let secret: [u8; TOKEN_BYTES] = decoded
        .try_into()
        .map_err(|_| FilesystemRegistryError::CapabilityInvalid)?;
    Ok(Sha256::digest(secret).into())
}

fn token_matches(left: &[u8; 32], right: &[u8; 32]) -> bool {
    bool::from(left.ct_eq(right))
}

fn admit_capability(state: &mut RegistryState, owner: SessionIdentity) {
    while state
        .capabilities
        .iter()
        .filter(|record| record.owner == owner)
        .count()
        >= MAX_CAPABILITIES_PER_SESSION
    {
        remove_oldest_capability(state, Some(owner));
    }
    while state.capabilities.len() >= MAX_CAPABILITIES_GLOBAL {
        remove_oldest_capability(state, None);
    }
}

fn remove_oldest_capability(state: &mut RegistryState, owner: Option<SessionIdentity>) {
    if let Some(index) = state
        .capabilities
        .iter()
        .enumerate()
        .filter(|(_, record)| owner.is_none_or(|owner| record.owner == owner))
        .min_by_key(|(_, record)| record.last_seen)
        .map(|(index, _)| index)
    {
        state.capabilities.remove(index);
    }
}

fn admit_cursor(state: &mut RegistryState, owner: SessionIdentity) {
    while state
        .cursors
        .iter()
        .filter(|record| record.owner == owner)
        .count()
        >= MAX_CURSORS_PER_SESSION
    {
        remove_oldest_cursor(state, Some(owner));
    }
    while state.cursors.len() >= MAX_CURSORS_GLOBAL {
        remove_oldest_cursor(state, None);
    }
}

fn remove_oldest_cursor(state: &mut RegistryState, owner: Option<SessionIdentity>) {
    if let Some(index) = state
        .cursors
        .iter()
        .enumerate()
        .filter(|(_, record)| owner.is_none_or(|owner| record.owner == owner))
        .min_by_key(|(_, record)| record.last_seen)
        .map(|(index, _)| index)
    {
        state.cursors.remove(index);
    }
}

fn reap_expired(state: &mut RegistryState, now: Instant) {
    state.capabilities.retain(|record| {
        let (idle_ttl, absolute_ttl) = match record.value {
            Capability::Browse(_) => (BROWSE_IDLE_TTL, BROWSE_ABSOLUTE_TTL),
            Capability::Root { .. } => (ROOT_CAPABILITY_IDLE_TTL, ROOT_CAPABILITY_ABSOLUTE_TTL),
        };
        elapsed_valid(record.last_seen, idle_ttl, now)
            && absolute_valid(record.created_at, absolute_ttl, now)
    });
    state.cursors.retain(|cursor| {
        elapsed_valid(cursor.last_seen, CURSOR_IDLE_TTL, now)
            && absolute_valid(cursor.created_at, CURSOR_ABSOLUTE_TTL, now)
    });
}

fn elapsed_valid(last_seen: Instant, ttl: Duration, now: Instant) -> bool {
    now.checked_duration_since(last_seen)
        .is_some_and(|elapsed| elapsed < ttl)
}

fn absolute_valid(created_at: Instant, ttl: Duration, now: Instant) -> bool {
    now.checked_duration_since(created_at)
        .is_some_and(|elapsed| elapsed < ttl)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rootlight_cancel::Cancellation;
    use tempfile::TempDir;

    use super::*;

    fn identity(byte: u8) -> SessionIdentity {
        SessionIdentity::from_test_bytes([byte; 32])
    }

    fn browse_directory() -> (TempDir, BrowseDirectory) {
        let temporary = TempDir::new().expect("temporary directory exists");
        let directory = BrowseDirectory::open(temporary.path(), &Cancellation::new())
            .expect("temporary directory opens through VFS");
        (temporary, directory)
    }

    #[test]
    fn browse_tokens_are_session_bound_and_expire_absolutely() {
        let registry = FilesystemRegistry::new();
        let now = Instant::now();
        let (_temporary, directory) = browse_directory();
        let issued = registry
            .issue_browse(identity(1), directory, None, "root".to_owned(), None, now)
            .expect("browse token issues");

        assert!(
            registry
                .resolve_browse(identity(2), &issued.token, now)
                .is_err()
        );
        let idle_expired = now
            .checked_add(BROWSE_IDLE_TTL)
            .expect("test instant is representable");
        assert!(
            registry
                .resolve_browse(identity(1), &issued.token, idle_expired)
                .is_err()
        );

        let (_temporary, directory) = browse_directory();
        let issued = registry
            .issue_browse(identity(1), directory, None, "root".to_owned(), None, now)
            .expect("second browse token issues");
        let expired = now
            .checked_add(BROWSE_ABSOLUTE_TTL)
            .expect("test instant is representable");
        assert!(
            registry
                .resolve_browse(identity(1), &issued.token, expired)
                .is_err()
        );
    }

    #[test]
    fn global_capability_limit_evicts_one_record_before_admission() {
        let now = Instant::now();
        let (_temporary, directory) = browse_directory();
        let token = random_token().expect("opaque token issues");
        let node = Arc::new(BrowseNode {
            owner: identity(1),
            token,
            created_at: now,
            directory: Arc::new(directory),
            snapshot: Mutex::new(None),
            label: "root".to_owned(),
            parent: None,
            depth: 0,
        });
        let mut state = RegistryState {
            capabilities: (0..MAX_CAPABILITIES_GLOBAL)
                .map(|index| CapabilityRecord {
                    owner: identity(u8::try_from(index).expect("global limit fits in u8")),
                    created_at: now,
                    last_seen: now,
                    value: Capability::Browse(Arc::clone(&node)),
                })
                .collect(),
            cursors: Vec::new(),
        };

        admit_capability(&mut state, identity(255));
        assert_eq!(state.capabilities.len(), MAX_CAPABILITIES_GLOBAL - 1);
    }

    #[test]
    fn capability_limit_evicts_the_least_recently_used_record() {
        let registry = FilesystemRegistry::new();
        let now = Instant::now();
        let mut first_token = None;
        let mut directories = Vec::new();
        for index in 0..=MAX_CAPABILITIES_PER_SESSION {
            let temporary = TempDir::new().expect("temporary directory exists");
            fs::create_dir(temporary.path().join("child")).expect("fixture child exists");
            let directory = BrowseDirectory::open(temporary.path(), &Cancellation::new())
                .expect("temporary directory opens through VFS");
            let issued = registry
                .issue_browse(
                    identity(3),
                    directory,
                    None,
                    format!("root-{index}"),
                    None,
                    now,
                )
                .expect("bounded browse token issues");
            first_token.get_or_insert(issued.token);
            directories.push(temporary);
        }

        assert_eq!(
            registry.capability_count(identity(3)),
            MAX_CAPABILITIES_PER_SESSION
        );
        assert!(
            registry
                .resolve_browse(
                    identity(3),
                    first_token.as_deref().expect("first token is retained"),
                    now,
                )
                .is_err()
        );
    }

    #[test]
    fn root_capability_allows_only_same_idempotency_retry_and_cleanup_is_session_exact() {
        let registry = FilesystemRegistry::new();
        let now = Instant::now();
        let (_temporary, directory) = browse_directory();
        let issued = registry
            .issue_root(
                identity(4),
                RootAdmission::new(directory, "selected".to_owned()),
                now,
            )
            .expect("root capability issues");

        assert!(
            registry
                .bind_root(identity(5), &issued.token, [1; 32], now)
                .is_err()
        );
        let admission = registry
            .bind_root(identity(4), &issued.token, [1; 32], now)
            .expect("owner binds root capability");
        assert_eq!(admission.display_label(), "selected");
        assert!(admission.local_path().is_absolute());
        assert!(
            registry
                .bind_root(identity(4), &issued.token, [1; 32], now)
                .is_ok()
        );
        assert!(
            registry
                .bind_root(identity(4), &issued.token, [2; 32], now)
                .is_err()
        );

        let (_temporary, directory) = browse_directory();
        let browse = registry
            .issue_browse(identity(4), directory, None, "root".to_owned(), None, now)
            .expect("browse token issues");
        registry.clear_session(identity(5));
        assert!(
            registry
                .resolve_browse(identity(4), &browse.token, now)
                .is_ok()
        );
        registry.clear_session(identity(4));
        assert!(
            registry
                .resolve_browse(identity(4), &browse.token, now)
                .is_err()
        );
    }
}
