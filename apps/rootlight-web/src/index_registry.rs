//! Session-owned idempotency records for detached repository indexing.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rootlight_client::{OperationId, RepositoryIndex, RepositoryIndexMode};

use crate::{filesystem_registry::RootAdmission, session::SessionIdentity};

const INDEX_RECORD_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_INDEX_RECORDS_PER_SESSION: usize = 64;
const MAX_INDEX_RECORDS_GLOBAL: usize = 256;

pub(crate) struct IndexSubmission {
    operation: OperationId,
    mode: RepositoryIndexMode,
    admission: Arc<RootAdmission>,
    gate: tokio::sync::Mutex<()>,
    result: Mutex<Option<RepositoryIndex>>,
}

impl IndexSubmission {
    pub(crate) const fn operation(&self) -> OperationId {
        self.operation
    }

    pub(crate) const fn mode(&self) -> RepositoryIndexMode {
        self.mode
    }

    pub(crate) fn admission(&self) -> &RootAdmission {
        &self.admission
    }

    pub(crate) fn gate(&self) -> &tokio::sync::Mutex<()> {
        &self.gate
    }

    pub(crate) fn result(&self) -> Result<Option<RepositoryIndex>, IndexRegistryError> {
        self.result
            .lock()
            .map(|result| result.clone())
            .map_err(|_| IndexRegistryError::Unavailable)
    }

    pub(crate) fn record_result(&self, result: RepositoryIndex) -> Result<(), IndexRegistryError> {
        let mut stored = self
            .result
            .lock()
            .map_err(|_| IndexRegistryError::Unavailable)?;
        *stored = Some(result);
        Ok(())
    }
}

struct IndexRecord {
    owner: SessionIdentity,
    idempotency_digest: [u8; 32],
    request_fingerprint: [u8; 32],
    created_at: Instant,
    last_seen: Instant,
    submission: Arc<IndexSubmission>,
}

pub(crate) struct NewIndexSubmission {
    pub(crate) owner: SessionIdentity,
    pub(crate) idempotency_digest: [u8; 32],
    pub(crate) request_fingerprint: [u8; 32],
    pub(crate) operation: OperationId,
    pub(crate) mode: RepositoryIndexMode,
    pub(crate) admission: Arc<RootAdmission>,
    pub(crate) now: Instant,
}

#[derive(Default)]
struct RegistryState {
    records: Vec<IndexRecord>,
}

pub(crate) struct IndexRegistry {
    state: Mutex<RegistryState>,
}

impl IndexRegistry {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(RegistryState::default()),
        }
    }

    pub(crate) fn find(
        &self,
        owner: SessionIdentity,
        idempotency_digest: &[u8; 32],
        request_fingerprint: &[u8; 32],
        now: Instant,
    ) -> Result<Option<Arc<IndexSubmission>>, IndexRegistryError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| IndexRegistryError::Unavailable)?;
        reap_expired(&mut state, now);
        let Some(record) = state.records.iter_mut().find(|record| {
            record.owner == owner && record.idempotency_digest == *idempotency_digest
        }) else {
            return Ok(None);
        };
        if record.request_fingerprint != *request_fingerprint {
            return Err(IndexRegistryError::Conflict);
        }
        record.last_seen = now;
        Ok(Some(Arc::clone(&record.submission)))
    }

    pub(crate) fn insert_or_find(
        &self,
        candidate: NewIndexSubmission,
    ) -> Result<Arc<IndexSubmission>, IndexRegistryError> {
        let NewIndexSubmission {
            owner,
            idempotency_digest,
            request_fingerprint,
            operation,
            mode,
            admission,
            now,
        } = candidate;
        let mut state = self
            .state
            .lock()
            .map_err(|_| IndexRegistryError::Unavailable)?;
        reap_expired(&mut state, now);
        if let Some(record) = state
            .records
            .iter_mut()
            .find(|record| record.owner == owner && record.idempotency_digest == idempotency_digest)
        {
            if record.request_fingerprint != request_fingerprint {
                return Err(IndexRegistryError::Conflict);
            }
            record.last_seen = now;
            return Ok(Arc::clone(&record.submission));
        }
        let session_count = state
            .records
            .iter()
            .filter(|record| record.owner == owner)
            .count();
        if session_count >= MAX_INDEX_RECORDS_PER_SESSION
            || state.records.len() >= MAX_INDEX_RECORDS_GLOBAL
        {
            return Err(IndexRegistryError::LimitReached);
        }
        let submission = Arc::new(IndexSubmission {
            operation,
            mode,
            admission,
            gate: tokio::sync::Mutex::new(()),
            result: Mutex::new(None),
        });
        state.records.push(IndexRecord {
            owner,
            idempotency_digest,
            request_fingerprint,
            created_at: now,
            last_seen: now,
            submission: Arc::clone(&submission),
        });
        Ok(submission)
    }

    pub(crate) fn find_operation(
        &self,
        owner: SessionIdentity,
        operation: OperationId,
        now: Instant,
    ) -> Result<Arc<IndexSubmission>, IndexRegistryError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| IndexRegistryError::Unavailable)?;
        reap_expired(&mut state, now);
        let mut matching_record = None;
        for record in &mut state.records {
            if record.owner != owner {
                continue;
            }
            let semantic_operation = record
                .submission
                .result()?
                .and_then(|result| result.semantic_operation);
            if record.submission.operation() == operation || semantic_operation == Some(operation) {
                matching_record = Some(record);
                break;
            }
        }
        let record = matching_record.ok_or(IndexRegistryError::NotFound)?;
        record.last_seen = now;
        Ok(Arc::clone(&record.submission))
    }

    pub(crate) fn clear_session(&self, owner: SessionIdentity) {
        if let Ok(mut state) = self.state.lock() {
            state.records.retain(|record| record.owner != owner);
        }
    }

    pub(crate) fn clear_sessions(&self, owners: &[SessionIdentity]) {
        if let Ok(mut state) = self.state.lock() {
            state
                .records
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
            state.records.clear();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexRegistryError {
    Conflict,
    LimitReached,
    NotFound,
    Unavailable,
}

fn reap_expired(state: &mut RegistryState, now: Instant) {
    state.records.retain(|record| {
        now.checked_duration_since(record.created_at)
            .is_some_and(|elapsed| elapsed < INDEX_RECORD_TTL)
    });
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::Cancellation;
    use rootlight_client::{OperationState, RepositoryId};
    use rootlight_vfs::BrowseDirectory;
    use tempfile::TempDir;

    use super::*;

    fn identity(byte: u8) -> SessionIdentity {
        SessionIdentity::from_test_bytes([byte; 32])
    }

    fn admission() -> (TempDir, Arc<RootAdmission>) {
        let temporary = TempDir::new().expect("temporary root exists");
        let directory = BrowseDirectory::open(temporary.path(), &Cancellation::new())
            .expect("temporary root opens");
        (
            temporary,
            Arc::new(RootAdmission::new(directory, "selected root".to_owned())),
        )
    }

    #[test]
    fn retries_reuse_one_operation_and_conflicting_payloads_fail_closed() {
        let registry = IndexRegistry::new();
        let (temporary, admission) = admission();
        let now = Instant::now();
        let first = registry
            .insert_or_find(NewIndexSubmission {
                owner: identity(1),
                idempotency_digest: [2; 32],
                request_fingerprint: [3; 32],
                operation: OperationId::from_bytes([4; 16]),
                mode: RepositoryIndexMode::Auto,
                admission: Arc::clone(&admission),
                now,
            })
            .expect("first request inserts");
        let retry = registry
            .insert_or_find(NewIndexSubmission {
                owner: identity(1),
                idempotency_digest: [2; 32],
                request_fingerprint: [3; 32],
                operation: OperationId::from_bytes([5; 16]),
                mode: RepositoryIndexMode::Auto,
                admission,
                now,
            })
            .expect("matching retry resolves");
        assert!(Arc::ptr_eq(&first, &retry));
        assert_eq!(retry.operation(), OperationId::from_bytes([4; 16]));
        assert!(matches!(
            registry.find(identity(1), &[2; 32], &[9; 32], now),
            Err(IndexRegistryError::Conflict)
        ));
        assert!(matches!(
            registry.find_operation(identity(2), first.operation(), now),
            Err(IndexRegistryError::NotFound)
        ));
        let semantic_operation = OperationId::from_bytes([8; 16]);
        first
            .record_result(RepositoryIndex {
                repository: RepositoryId::from_bytes([7; 16]),
                operation: first.operation(),
                semantic_operation: Some(semantic_operation),
                state: OperationState::Succeeded,
                revision: 2,
                mode: RepositoryIndexMode::Auto,
                parent_generation: None,
                published_generation: None,
                discovered_inputs: 0,
                indexed_files: 0,
                entities: 0,
                elapsed_micros: 0,
                estimated_disk_bytes: 0,
                diagnostics: Vec::new(),
            })
            .expect("result records");
        assert!(
            registry
                .find_operation(identity(1), semantic_operation, now)
                .is_ok()
        );
        assert!(matches!(
            registry.find_operation(identity(2), semantic_operation, now),
            Err(IndexRegistryError::NotFound)
        ));
        assert_eq!(first.admission().local_path(), temporary.path());
    }

    #[test]
    fn records_expire_and_session_cleanup_is_exact() {
        let registry = IndexRegistry::new();
        let (_first_root, first_admission) = admission();
        let (_second_root, second_admission) = admission();
        let now = Instant::now();
        let first = registry
            .insert_or_find(NewIndexSubmission {
                owner: identity(1),
                idempotency_digest: [1; 32],
                request_fingerprint: [2; 32],
                operation: OperationId::from_bytes([3; 16]),
                mode: RepositoryIndexMode::Structural,
                admission: first_admission,
                now,
            })
            .expect("first inserts");
        let second = registry
            .insert_or_find(NewIndexSubmission {
                owner: identity(2),
                idempotency_digest: [4; 32],
                request_fingerprint: [5; 32],
                operation: OperationId::from_bytes([6; 16]),
                mode: RepositoryIndexMode::Deep,
                admission: second_admission,
                now,
            })
            .expect("second inserts");

        registry.clear_session(identity(1));
        assert!(matches!(
            registry.find_operation(identity(1), first.operation(), now),
            Err(IndexRegistryError::NotFound)
        ));
        assert!(
            registry
                .find_operation(identity(2), second.operation(), now)
                .is_ok()
        );
        let expired = now + INDEX_RECORD_TTL + Duration::from_millis(1);
        assert!(matches!(
            registry.find_operation(identity(2), second.operation(), expired),
            Err(IndexRegistryError::NotFound)
        ));
    }
}
