//! Durable bounded operation lifecycle and catalog-writer arbitration.
//!
//! This crate owns immutable submission metadata, monotonic lifecycle updates,
//! restart classification, bounded SQLite retention, and one-writer locking.

#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    fs::{File, TryLockError},
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_error::PublicError;
use rootlight_ids::OperationId;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction,
    config::DbConfig,
    hooks::{AuthAction, Authorization},
    params,
};
use sha2::{Digest as _, Sha256};

/// Maximum terminal operation records retained after pruning.
pub const MAX_OPERATION_HISTORY: usize = 10_000;
/// Maximum serialized public error retained for one terminal operation.
pub const MAX_PUBLIC_ERROR_BYTES: usize = 16 * 1024;
/// Minimum bundled SQLite version required by the P1 catalog.
pub const MIN_SQLITE_VERSION_NUMBER: i32 = 3_051_003;
/// Current operation-journal schema version.
pub const OPERATION_SCHEMA_VERSION: u32 = 3;
/// SQLite application identifier for Rootlight's per-user catalog.
pub const CATALOG_APPLICATION_ID: u32 = 0x5254_4c54;
/// Bounded wait for transient catalog contention.
pub const CATALOG_BUSY_TIMEOUT: Duration = Duration::from_millis(250);

const OPERATION_SCHEMA_MIGRATION_ID: u32 = 3;
// SHA-256 of the three canonical schema statements in `migration_checksum_input`;
// change this reviewed ledger value rather than silently accepting schema drift.
const OPERATION_SCHEMA_MIGRATION_CHECKSUM: [u8; 32] = [
    0x0a, 0xc6, 0x43, 0xfe, 0xca, 0x7f, 0x37, 0x08, 0x63, 0x21, 0x22, 0x07, 0x36, 0xf3, 0xa1, 0xa0,
    0x39, 0x65, 0x59, 0x27, 0x65, 0xb6, 0xe0, 0xf5, 0xc0, 0x05, 0xdd, 0xc0, 0x0a, 0x4b, 0x53, 0xec,
];
// Version-two catalogs must authenticate against their original reviewed
// ledger entry before any migration rewrites durable rows.
const VERSION_TWO_SCHEMA_MIGRATION_ID: u32 = 2;
const VERSION_TWO_SCHEMA_MIGRATION_CHECKSUM: [u8; 32] = [
    0x38, 0x93, 0xf3, 0xf0, 0x33, 0x07, 0xb8, 0x8e, 0x9c, 0x15, 0x64, 0xa5, 0x53, 0x6f, 0x76, 0x92,
    0x55, 0xe3, 0xbd, 0xc9, 0x36, 0x57, 0x73, 0x57, 0x4e, 0xfd, 0xfc, 0x8a, 0x09, 0xe6, 0x66, 0xfe,
];
const CONTROL_PROBE_PLAN_HASH: [u8; 32] = [0; 32];
const SYSTEM_CLIENT_INSTANCE_ID: [u8; 16] = [0; 16];

/// Checked client-declared identity carried over an OS-authorized local connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClientInstanceId([u8; 16]);

impl ClientInstanceId {
    /// Reserved identity used only for internal legacy and standalone work.
    pub const SYSTEM: Self = Self(SYSTEM_CLIENT_INSTANCE_ID);

    /// Creates a non-reserved client-declared identity.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::InvalidClientInstanceId`] for the reserved zero value.
    pub fn new(bytes: [u8; 16]) -> Result<Self, OperationError> {
        if bytes == SYSTEM_CLIENT_INSTANCE_ID {
            return Err(OperationError::InvalidClientInstanceId);
        }
        Ok(Self(bytes))
    }

    /// Reconstructs an identity, including the reserved internal identity.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns the stable binary representation.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

/// Canonical digest of immutable operation-plan inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlanHash([u8; 32]);

impl PlanHash {
    /// Creates a plan hash from its canonical digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the canonical digest bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Durable operation kind supported by the M04 infrastructure slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperationKind {
    /// Deterministic infrastructure work used to prove lifecycle behavior.
    ControlProbe,
}

impl OperationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ControlProbe => "control_probe",
        }
    }

    fn parse(value: &str) -> Result<Self, OperationError> {
        match value {
            "control_probe" => Ok(Self::ControlProbe),
            _ => Err(OperationError::CorruptState),
        }
    }
}

/// Monotonic stage within an operation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperationStage {
    /// The operation is durably accepted.
    Accepted,
    /// The operation owns execution capacity.
    Executing,
    /// The operation is releasing temporary resources.
    Cleanup,
}

impl OperationStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Executing => "executing",
            Self::Cleanup => "cleanup",
        }
    }

    fn parse(value: &str) -> Result<Self, OperationError> {
        match value {
            "accepted" => Ok(Self::Accepted),
            "executing" => Ok(Self::Executing),
            "cleanup" => Ok(Self::Cleanup),
            _ => Err(OperationError::CorruptState),
        }
    }
}

/// Stable recovery classification assigned to terminal interrupted work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecoveryClass {
    /// Recovery classification does not apply.
    NotApplicable,
    /// Nonterminal work was interrupted by restart or shutdown.
    InterruptedByRestart,
    /// The submitted deadline elapsed.
    DeadlineElapsed,
    /// An attached-client lease elapsed.
    LeaseExpired,
}

impl RecoveryClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotApplicable => "not_applicable",
            Self::InterruptedByRestart => "interrupted_by_restart",
            Self::DeadlineElapsed => "deadline_elapsed",
            Self::LeaseExpired => "lease_expired",
        }
    }

    fn parse(value: &str) -> Result<Self, OperationError> {
        match value {
            "not_applicable" => Ok(Self::NotApplicable),
            "interrupted_by_restart" => Ok(Self::InterruptedByRestart),
            "deadline_elapsed" => Ok(Self::DeadlineElapsed),
            "lease_expired" => Ok(Self::LeaseExpired),
            _ => Err(OperationError::CorruptState),
        }
    }
}

/// Immutable metadata accepted with one durable operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationSubmission {
    /// Stable operation handle.
    pub operation: OperationId,
    /// Bounded operation kind.
    pub kind: OperationKind,
    /// Canonical plan digest.
    pub plan_hash: PlanHash,
    /// Authenticated submitting client identity.
    pub owner: ClientInstanceId,
    /// Whether work may outlive the submitting client lease.
    pub detached: bool,
    /// Optional wall-clock deadline used for admission and recovery.
    pub deadline_unix_ms: Option<u64>,
    /// Required wall-clock lease expiry for attached work.
    pub lease_expires_unix_ms: Option<u64>,
}

/// Retry comparison for a newly prepared operation deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineRetry {
    /// Requires the submitted absolute deadline to match durable metadata exactly.
    Exact,
    /// Matches detached work whose identical relative timeout was re-anchored.
    ReanchoredRelative {
        /// Original relative timeout, before conversion to an audit wall-clock deadline.
        timeout_ms: u64,
    },
}

impl DeadlineRetry {
    const fn relative_timeout_ms(self) -> Option<u64> {
        match self {
            Self::Exact => None,
            Self::ReanchoredRelative { timeout_ms } => Some(timeout_ms),
        }
    }
}

impl OperationSubmission {
    /// Creates a checked immutable submission.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::InvalidSubmission`] when detached and lease
    /// metadata disagree or an explicit timestamp is zero.
    pub fn new(
        operation: OperationId,
        kind: OperationKind,
        plan_hash: PlanHash,
        owner: ClientInstanceId,
        detached: bool,
        deadline_unix_ms: Option<u64>,
        lease_expires_unix_ms: Option<u64>,
    ) -> Result<Self, OperationError> {
        if deadline_unix_ms == Some(0)
            || lease_expires_unix_ms == Some(0)
            || detached == lease_expires_unix_ms.is_some()
        {
            return Err(OperationError::InvalidSubmission);
        }
        Ok(Self {
            operation,
            kind,
            plan_hash,
            owner,
            detached,
            deadline_unix_ms,
            lease_expires_unix_ms,
        })
    }

    /// Creates the internal legacy control-probe submission.
    #[must_use]
    pub const fn control_probe(operation: OperationId) -> Self {
        Self {
            operation,
            kind: OperationKind::ControlProbe,
            plan_hash: PlanHash::from_bytes(CONTROL_PROBE_PLAN_HASH),
            owner: ClientInstanceId::SYSTEM,
            detached: true,
            deadline_unix_ms: None,
            lease_expires_unix_ms: None,
        }
    }
}

/// Durable lifecycle state for one long-running operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperationState {
    /// Accepted and waiting for execution capacity.
    Queued,
    /// Work has started.
    Running,
    /// Cooperative cancellation has been requested.
    Cancelling,
    /// Work completed and its durable effects are valid.
    Succeeded,
    /// Work failed with a stable public error.
    Failed,
    /// Cooperative cancellation completed before publication commit.
    Cancelled,
    /// Work did not reach a valid terminal result before restart or shutdown.
    Interrupted,
}

impl OperationState {
    /// Reports whether no further state transition is legal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    fn parse(value: &str) -> Result<Self, OperationError> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "cancelling" => Ok(Self::Cancelling),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(OperationError::CorruptState),
        }
    }
}

/// Monotonic progress associated with an operation state revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Completed bounded work units.
    pub completed: u32,
    /// Total bounded work units, zero when not yet known.
    pub total: u32,
}

impl Progress {
    /// Creates progress when completion does not exceed a known total.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::InvalidProgress`] for inconsistent values.
    pub const fn new(completed: u32, total: u32) -> Result<Self, OperationError> {
        if total != 0 && completed > total {
            return Err(OperationError::InvalidProgress);
        }
        Ok(Self { completed, total })
    }
}

/// Durable source-redacted state for one operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    /// Stable operation handle.
    pub operation: OperationId,
    /// Submitted operation kind.
    pub kind: OperationKind,
    /// Canonical operation-plan digest.
    pub plan_hash: PlanHash,
    /// Authenticated submitting client identity.
    pub owner: ClientInstanceId,
    /// Whether work may outlive its submitting client.
    pub detached: bool,
    /// Optional durable wall-clock deadline.
    pub deadline_unix_ms: Option<u64>,
    /// Optional attached-client lease expiry.
    pub lease_expires_unix_ms: Option<u64>,
    /// Current lifecycle state.
    pub state: OperationState,
    /// Current monotonic operation stage.
    pub stage: OperationStage,
    /// Whether cancellation durably won the request race.
    pub cancellation_requested: bool,
    /// First durable cancellation reason.
    pub cancellation_reason: Option<CancellationReason>,
    /// Restart or expiry classification.
    pub recovery_class: RecoveryClass,
    /// Monotonically increasing state, stage, or progress revision.
    pub revision: u64,
    /// Monotonic progress snapshot.
    pub progress: Progress,
    /// Stable public terminal failure.
    pub error: Option<PublicError>,
    /// Reports that a persisted failure envelope exists after restart.
    pub has_persisted_error: bool,
}

/// Result of retry-safe durable submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionOutcome {
    /// Whether this call inserted the durable record.
    pub inserted: bool,
    /// Durable state after submission.
    pub operation: OperationRecord,
}

/// Result of a durable cancellation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationOutcome {
    /// Whether this call first established durable cancellation.
    pub accepted: bool,
    /// Durable record after the request.
    pub operation: OperationRecord,
}

/// Bounded operation counts for source-free health reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationCounts {
    /// Operations waiting for execution capacity.
    pub queued: u32,
    /// Operations currently executing.
    pub running: u32,
    /// Operations completing cancellation cleanup.
    pub cancelling: u32,
}

impl OperationCounts {
    /// Returns all durable nonterminal records.
    #[must_use]
    pub const fn active(self) -> u32 {
        self.queued
            .saturating_add(self.running)
            .saturating_add(self.cancelling)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogStorage {
    Persistent,
    Memory,
}

impl CatalogStorage {
    const fn journal_mode(self) -> &'static str {
        match self {
            Self::Persistent => "wal",
            Self::Memory => "memory",
        }
    }
}

/// Durable SQLite operation journal.
#[derive(Debug)]
pub struct OperationJournal {
    connection: Mutex<Connection>,
    cancellations: Mutex<BTreeMap<OperationId, Cancellation>>,
    errors: Mutex<BTreeMap<OperationId, PublicError>>,
}

impl OperationJournal {
    /// Opens or creates a journal and classifies abandoned nonterminal work.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError`] for SQLite version, schema, integrity, or recovery failures.
    pub fn open(path: &Path) -> Result<Self, OperationError> {
        let connection = Connection::open(path).map_err(map_sqlite_error)?;
        Self::initialize(connection, CatalogStorage::Persistent, unix_time_ms()?)
    }

    /// Opens an isolated in-memory journal for standalone composition and tests.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError`] for SQLite setup failures.
    pub fn open_in_memory() -> Result<Self, OperationError> {
        let connection = Connection::open_in_memory().map_err(map_sqlite_error)?;
        Self::initialize(connection, CatalogStorage::Memory, unix_time_ms()?)
    }

    fn initialize(
        mut connection: Connection,
        storage: CatalogStorage,
        now_unix_ms: u64,
    ) -> Result<Self, OperationError> {
        verify_sqlite(&connection)?;
        configure_catalog_connection(&connection, storage)?;
        migrate_schema(&mut connection, storage)?;
        validate_catalog_identity(&connection)?;
        install_catalog_authorizer(&connection)?;
        let journal = Self {
            connection: Mutex::new(connection),
            cancellations: Mutex::new(BTreeMap::new()),
            errors: Mutex::new(BTreeMap::new()),
        };
        journal.recover_nonterminal(now_unix_ms)?;
        journal.prune_to(MAX_OPERATION_HISTORY)?;
        Ok(journal)
    }

    /// Submits immutable operation metadata with retry-safe idempotency.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::SubmissionConflict`] when the operation ID is
    /// reused with different immutable metadata, or a typed storage error.
    pub fn submit(
        &self,
        submission: OperationSubmission,
    ) -> Result<SubmissionOutcome, OperationError> {
        self.submit_with_deadline_retry(submission, DeadlineRetry::Exact)
    }

    /// Submits immutable operation metadata with an explicit deadline-retry policy.
    ///
    /// Re-anchored matching is reserved for detached relative timeouts. It preserves
    /// the first durable absolute deadline while still rejecting timed/untimed reuse.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::SubmissionConflict`] when immutable metadata is
    /// incompatible, or a typed validation or storage error.
    pub fn submit_with_deadline_retry(
        &self,
        submission: OperationSubmission,
        deadline_retry: DeadlineRetry,
    ) -> Result<SubmissionOutcome, OperationError> {
        validate_submission_with_retry(submission, deadline_retry)?;
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        if let Some((existing, stored_timeout_ms)) =
            load_record_with_retry_intent(&transaction, submission.operation)?
        {
            if submission_matches(&existing, stored_timeout_ms, submission, deadline_retry) {
                transaction.commit().map_err(map_sqlite_error)?;
                return Ok(SubmissionOutcome {
                    inserted: false,
                    operation: existing,
                });
            }
            return Err(OperationError::SubmissionConflict);
        }
        let sequence = next_sequence(&transaction)?;
        transaction
            .execute(
                "INSERT INTO operations (
                    operation, kind, plan_hash, owner, detached, deadline_unix_ms,
                    relative_timeout_ms, lease_expires_unix_ms, state, stage, cancellation_requested,
                    cancellation_reason, recovery_class, revision, completed, total,
                    error_json, sequence
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'queued', 'accepted', 0,
                           NULL, 'not_applicable', 1, 0, 0, NULL, ?9)",
                params![
                    submission.operation.as_bytes().as_slice(),
                    submission.kind.as_str(),
                    submission.plan_hash.as_bytes().as_slice(),
                    submission.owner.as_bytes().as_slice(),
                    bool_to_i64(submission.detached),
                    optional_u64_to_i64(submission.deadline_unix_ms)?,
                    optional_u64_to_i64(deadline_retry.relative_timeout_ms())?,
                    optional_u64_to_i64(submission.lease_expires_unix_ms)?,
                    sequence,
                ],
            )
            .map_err(map_sqlite_error)?;
        transaction.commit().map_err(map_sqlite_error)?;
        self.lock_cancellations()?
            .insert(submission.operation, Cancellation::new());
        drop(connection);
        self.prune_to(MAX_OPERATION_HISTORY)?;
        Ok(SubmissionOutcome {
            inserted: true,
            operation: self.status(submission.operation)?,
        })
    }

    /// Returns an existing retry-compatible record without inserting new work.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::SubmissionConflict`] when immutable metadata differs,
    /// or [`OperationError::NotFound`] when capacity is still required for a new record.
    pub fn retry_status(
        &self,
        submission: OperationSubmission,
    ) -> Result<OperationRecord, OperationError> {
        self.retry_status_with_deadline_retry(submission, DeadlineRetry::Exact)
    }

    /// Returns retry-compatible work using an explicit deadline-retry policy.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::SubmissionConflict`] when immutable metadata is
    /// incompatible, or [`OperationError::NotFound`] when no durable record exists.
    pub fn retry_status_with_deadline_retry(
        &self,
        submission: OperationSubmission,
        deadline_retry: DeadlineRetry,
    ) -> Result<OperationRecord, OperationError> {
        validate_submission_with_retry(submission, deadline_retry)?;
        let connection = self.lock_connection()?;
        let (existing, stored_timeout_ms) =
            load_record_with_retry_intent(&connection, submission.operation)?
                .ok_or(OperationError::NotFound)?;
        if submission_matches(&existing, stored_timeout_ms, submission, deadline_retry) {
            Ok(existing)
        } else {
            Err(OperationError::SubmissionConflict)
        }
    }

    /// Inserts a legacy internal control probe and returns its cancellation token.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::AlreadyExists`] for any reused operation ID.
    pub fn enqueue(&self, operation: OperationId) -> Result<Cancellation, OperationError> {
        if self.status(operation).is_ok() {
            return Err(OperationError::AlreadyExists);
        }
        match self.submit(OperationSubmission::control_probe(operation)) {
            Ok(_) => self.cancellation_token(operation),
            Err(OperationError::SubmissionConflict) => Err(OperationError::AlreadyExists),
            Err(error) => Err(error),
        }
    }

    /// Applies one legal revision-guarded state transition.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid, cancelled, stale, or unknown work.
    pub fn transition(
        &self,
        operation: OperationId,
        next: OperationState,
        error: Option<&PublicError>,
    ) -> Result<OperationRecord, OperationError> {
        let current = self.status(operation)?;
        if current.state == OperationState::Interrupted && next == OperationState::Succeeded {
            return Ok(current);
        }
        if current.cancellation_requested && matches!(next, OperationState::Succeeded) {
            return Err(OperationError::CancellationWon);
        }
        if !legal_transition(current.state, next) {
            return Err(OperationError::IllegalTransition {
                from: current.state,
                to: next,
            });
        }
        if next == OperationState::Failed && error.is_none()
            || next != OperationState::Failed && error.is_some()
        {
            return Err(OperationError::InvalidTerminalError);
        }
        let error_json = error.map(serialize_public_error).transpose()?;
        let revision = next_revision(current.revision)?;
        let updated = {
            let connection = self.lock_connection()?;
            connection
                .execute(
                    "UPDATE operations
                     SET state = ?1, revision = ?2, error_json = ?3
                     WHERE operation = ?4 AND revision = ?5
                       AND NOT (?1 = 'succeeded' AND cancellation_requested = 1)",
                    params![
                        next.as_str(),
                        u64_to_i64(revision)?,
                        error_json,
                        operation.as_bytes().as_slice(),
                        u64_to_i64(current.revision)?,
                    ],
                )
                .map_err(map_sqlite_error)?
        };
        if updated != 1 {
            let observed = self.status(operation)?;
            if observed.state == OperationState::Interrupted && next == OperationState::Succeeded {
                return Ok(observed);
            }
            if observed.cancellation_requested && next == OperationState::Succeeded {
                return Err(OperationError::CancellationWon);
            }
            return Err(OperationError::ConcurrentUpdate);
        }
        if let Some(error) = error {
            self.lock_errors()?.insert(operation, error.clone());
        } else {
            self.lock_errors()?.remove(&operation);
        }
        if next.is_terminal() {
            self.lock_cancellations()?.remove(&operation);
            self.prune_to(MAX_OPERATION_HISTORY)?;
        }
        self.status(operation)
    }

    /// Atomically marks queued work as running at its executing stage.
    ///
    /// Worker scheduling depends on both fields advancing together. A crash must
    /// never leave durable `running/accepted` work that no worker can own.
    ///
    /// # Errors
    ///
    /// Returns a typed error for stale, cancelled, terminal, or unknown work.
    pub fn start_execution(
        &self,
        operation: OperationId,
    ) -> Result<OperationRecord, OperationError> {
        let current = self.status(operation)?;
        if current.state != OperationState::Queued || current.stage != OperationStage::Accepted {
            return Err(OperationError::IllegalTransition {
                from: current.state,
                to: OperationState::Running,
            });
        }
        let revision = next_revision(current.revision)?;
        let connection = self.lock_connection()?;
        let updated = connection
            .execute(
                "UPDATE operations
                 SET state = 'running', stage = 'executing', revision = ?1
                 WHERE operation = ?2 AND revision = ?3
                   AND state = 'queued' AND stage = 'accepted'
                   AND cancellation_requested = 0",
                params![
                    u64_to_i64(revision)?,
                    operation.as_bytes().as_slice(),
                    u64_to_i64(current.revision)?,
                ],
            )
            .map_err(map_sqlite_error)?;
        drop(connection);
        if updated != 1 {
            let observed = self.status(operation)?;
            if observed.cancellation_requested {
                return Err(OperationError::CancellationWon);
            }
            return Err(OperationError::ConcurrentUpdate);
        }
        self.status(operation)
    }

    /// Advances the monotonic operation stage.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::InvalidStage`] for regression, terminal work, or
    /// a stage that does not agree with the lifecycle state.
    pub fn update_stage(
        &self,
        operation: OperationId,
        stage: OperationStage,
    ) -> Result<OperationRecord, OperationError> {
        let current = self.status(operation)?;
        if current.state.is_terminal()
            || stage <= current.stage
            || stage == OperationStage::Executing && current.state != OperationState::Running
            || stage == OperationStage::Cleanup
                && !matches!(
                    current.state,
                    OperationState::Running | OperationState::Cancelling
                )
        {
            return Err(OperationError::InvalidStage);
        }
        let revision = next_revision(current.revision)?;
        let connection = self.lock_connection()?;
        let updated = connection
            .execute(
                "UPDATE operations SET stage = ?1, revision = ?2
                 WHERE operation = ?3 AND revision = ?4",
                params![
                    stage.as_str(),
                    u64_to_i64(revision)?,
                    operation.as_bytes().as_slice(),
                    u64_to_i64(current.revision)?,
                ],
            )
            .map_err(map_sqlite_error)?;
        drop(connection);
        if updated != 1 {
            return Err(OperationError::ConcurrentUpdate);
        }
        self.status(operation)
    }

    /// Advances progress without allowing completed units to move backward.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid, regressing, terminal, or unknown work.
    pub fn update_progress(
        &self,
        operation: OperationId,
        progress: Progress,
    ) -> Result<OperationRecord, OperationError> {
        let current = self.status(operation)?;
        if current.state == OperationState::Interrupted {
            return Ok(current);
        }
        if current.state.is_terminal()
            || progress.completed < current.progress.completed
            || current.progress.total != 0 && current.progress.total != progress.total
        {
            return Err(OperationError::InvalidProgress);
        }
        let revision = next_revision(current.revision)?;
        let connection = self.lock_connection()?;
        let updated = connection
            .execute(
                "UPDATE operations SET revision = ?1, completed = ?2, total = ?3
                 WHERE operation = ?4 AND revision = ?5",
                params![
                    u64_to_i64(revision)?,
                    progress.completed,
                    progress.total,
                    operation.as_bytes().as_slice(),
                    u64_to_i64(current.revision)?,
                ],
            )
            .map_err(map_sqlite_error)?;
        drop(connection);
        if updated != 1 {
            let observed = self.status(operation)?;
            if observed.state == OperationState::Interrupted {
                return Ok(observed);
            }
            return Err(OperationError::ConcurrentUpdate);
        }
        self.status(operation)
    }

    /// Durably requests cancellation before signalling in-memory workers.
    ///
    /// Queued work becomes terminal immediately; running work enters cleanup.
    /// Repeated or terminal requests are idempotent and return `accepted = false`.
    ///
    /// # Errors
    ///
    /// Returns a typed error for unknown work, unsupported reasons, or storage failure.
    pub fn request_cancellation(
        &self,
        operation: OperationId,
        reason: CancellationReason,
    ) -> Result<CancellationOutcome, OperationError> {
        let reason_text = cancellation_reason_as_str(reason)?;
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        let current = load_record(&transaction, operation)?.ok_or(OperationError::NotFound)?;
        if current.state.is_terminal() || current.cancellation_requested {
            transaction.commit().map_err(map_sqlite_error)?;
            return Ok(CancellationOutcome {
                accepted: false,
                operation: current,
            });
        }
        let next_state = match current.state {
            OperationState::Queued => OperationState::Cancelled,
            OperationState::Running => OperationState::Cancelling,
            OperationState::Cancelling => OperationState::Cancelling,
            OperationState::Succeeded
            | OperationState::Failed
            | OperationState::Cancelled
            | OperationState::Interrupted => unreachable!("terminal states returned above"),
        };
        let revision = next_revision(current.revision)?;
        let updated = transaction
            .execute(
                "UPDATE operations
                 SET state = ?1, cancellation_requested = 1,
                     cancellation_reason = ?2, revision = ?3
                 WHERE operation = ?4 AND revision = ?5
                   AND cancellation_requested = 0
                   AND state IN ('queued', 'running', 'cancelling')",
                params![
                    next_state.as_str(),
                    reason_text,
                    u64_to_i64(revision)?,
                    operation.as_bytes().as_slice(),
                    u64_to_i64(current.revision)?,
                ],
            )
            .map_err(map_sqlite_error)?;
        if updated != 1 {
            return Err(OperationError::ConcurrentUpdate);
        }
        transaction.commit().map_err(map_sqlite_error)?;
        drop(connection);
        let token = self.cancellation_token(operation)?;
        let _ = token.cancel(reason);
        if next_state.is_terminal() {
            self.lock_cancellations()?.remove(&operation);
            self.prune_to(MAX_OPERATION_HISTORY)?;
        }
        Ok(CancellationOutcome {
            accepted: true,
            operation: self.status(operation)?,
        })
    }

    /// Requests client cancellation and returns acknowledgement plus state.
    ///
    /// # Errors
    ///
    /// Returns a typed journal error.
    pub fn cancel(
        &self,
        operation: OperationId,
    ) -> Result<(bool, OperationRecord), OperationError> {
        let outcome = self.request_cancellation(operation, CancellationReason::ClientRequest)?;
        Ok((outcome.accepted, outcome.operation))
    }

    /// Returns the in-memory cancellation notification for active work.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::NotFound`] when no active token exists.
    pub fn cancellation_token(
        &self,
        operation: OperationId,
    ) -> Result<Cancellation, OperationError> {
        self.lock_cancellations()?
            .get(&operation)
            .cloned()
            .ok_or(OperationError::NotFound)
    }

    #[cfg(test)]
    fn renew_lease(
        &self,
        operation: OperationId,
        owner: ClientInstanceId,
        expiry_unix_ms: u64,
    ) -> Result<OperationRecord, OperationError> {
        let connection = self.lock_connection()?;
        let current = load_record(&connection, operation)?.ok_or(OperationError::NotFound)?;
        if current.detached || current.owner != owner {
            return Err(OperationError::LeaseOwnerMismatch);
        }
        if current.cancellation_requested {
            return Err(OperationError::CancellationWon);
        }
        if current.lease_expires_unix_ms == Some(expiry_unix_ms) {
            return Ok(current);
        }
        if current.state.is_terminal() {
            return Err(OperationError::LeaseOwnerMismatch);
        }
        if expiry_unix_ms == 0 || expiry_unix_ms <= unix_time_ms()? {
            return Err(OperationError::InvalidLease);
        }
        if current
            .lease_expires_unix_ms
            .is_some_and(|existing| expiry_unix_ms < existing)
        {
            return Err(OperationError::InvalidLease);
        }
        let revision = next_revision(current.revision)?;
        let updated = connection
            .execute(
                "UPDATE operations SET lease_expires_unix_ms = ?1, revision = ?2
                 WHERE operation = ?3 AND revision = ?4 AND detached = 0 AND owner = ?5",
                params![
                    u64_to_i64(expiry_unix_ms)?,
                    u64_to_i64(revision)?,
                    operation.as_bytes().as_slice(),
                    u64_to_i64(current.revision)?,
                    owner.as_bytes().as_slice(),
                ],
            )
            .map_err(map_sqlite_error)?;
        if updated != 1 {
            return Err(OperationError::ConcurrentUpdate);
        }
        drop(connection);
        self.status(operation)
    }

    /// Interrupts active work after its process-local deadline elapses.
    ///
    /// The process-local scheduler decides when the deadline is due. Persisted
    /// wall-clock timestamps remain audit and restart-classification metadata.
    ///
    /// # Errors
    ///
    /// Returns a typed storage or concurrency error.
    pub fn interrupt_deadline(
        &self,
        operation: OperationId,
    ) -> Result<OperationRecord, OperationError> {
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        let current = load_record(&transaction, operation)?.ok_or(OperationError::NotFound)?;
        if current.state.is_terminal() || current.cancellation_requested {
            transaction.commit().map_err(map_sqlite_error)?;
            return Ok(current);
        }
        let updated = update_interrupted(&transaction, operation, RecoveryClass::DeadlineElapsed)?;
        if updated != 1 {
            return Err(OperationError::ConcurrentUpdate);
        }
        transaction.commit().map_err(map_sqlite_error)?;
        if let Some(token) = self.lock_cancellations()?.remove(&operation) {
            let _ = token.cancel(CancellationReason::DeadlineExceeded);
        }
        drop(connection);
        self.prune_to(MAX_OPERATION_HISTORY)?;
        self.status(operation)
    }

    /// Interrupts an attached operation when the selected lease expiry wins.
    ///
    /// The expected persisted expiry prevents an already-selected stale timer from
    /// interrupting a lease that was renewed before this transition committed.
    ///
    /// # Errors
    ///
    /// Returns a typed storage or concurrency error.
    pub fn interrupt_lease(
        &self,
        operation: OperationId,
        expected_expiry_unix_ms: u64,
    ) -> Result<OperationRecord, OperationError> {
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        let current = load_record(&transaction, operation)?.ok_or(OperationError::NotFound)?;
        if current.state.is_terminal()
            || current.cancellation_requested
            || current.lease_expires_unix_ms != Some(expected_expiry_unix_ms)
        {
            transaction.commit().map_err(map_sqlite_error)?;
            return Ok(current);
        }
        if current.detached {
            return Err(OperationError::CorruptState);
        }
        let updated = transaction
            .execute(
                "UPDATE operations
                 SET state = 'interrupted', recovery_class = 'lease_expired',
                     revision = revision + 1
                 WHERE operation = ?1
                   AND lease_expires_unix_ms = ?2
                   AND cancellation_requested = 0
                   AND state IN ('queued', 'running', 'cancelling')",
                params![
                    operation.as_bytes().as_slice(),
                    u64_to_i64(expected_expiry_unix_ms)?,
                ],
            )
            .map_err(map_sqlite_error)?;
        if updated != 1 {
            return Err(OperationError::ConcurrentUpdate);
        }
        transaction.commit().map_err(map_sqlite_error)?;
        if let Some(token) = self.lock_cancellations()?.remove(&operation) {
            let _ = token.cancel(CancellationReason::ParentCancelled);
        }
        drop(connection);
        self.prune_to(MAX_OPERATION_HISTORY)?;
        self.status(operation)
    }

    /// Interrupts at most `max_records` remaining nonterminal operations.
    ///
    /// # Errors
    ///
    /// Returns a typed storage or bound error.
    pub fn interrupt_nonterminal(&self, max_records: usize) -> Result<u32, OperationError> {
        if max_records == 0 {
            return Ok(0);
        }
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        let ids = select_operation_ids(
            &transaction,
            "state IN ('queued', 'running', 'cancelling')",
            max_records,
            &[],
        )?;
        let mut interruptions = Vec::with_capacity(ids.len());
        for operation in ids {
            if update_interrupted(&transaction, operation, RecoveryClass::InterruptedByRestart)?
                == 1
            {
                interruptions.push(PendingInterruption {
                    operation,
                    reason: CancellationReason::Shutdown,
                    lease_expires_unix_ms: None,
                });
            }
        }
        let mut cancellations = self.lock_cancellations()?;
        transaction.commit().map_err(map_sqlite_error)?;
        for interruption in &interruptions {
            if let Some(token) = cancellations.remove(&interruption.operation) {
                let _ = token.cancel(interruption.reason);
            }
        }
        drop(cancellations);
        drop(connection);
        self.prune_to(MAX_OPERATION_HISTORY)?;
        u32::try_from(interruptions.len()).map_err(|_| OperationError::CorruptState)
    }

    /// Checkpoints the SQLite write-ahead log before orderly shutdown.
    ///
    /// # Errors
    ///
    /// Returns a typed SQLite failure.
    pub fn checkpoint(&self) -> Result<(), OperationError> {
        self.lock_connection()?
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(map_sqlite_error)
    }

    /// Loads one durable operation state.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::NotFound`] or a typed corruption/storage error.
    pub fn status(&self, operation: OperationId) -> Result<OperationRecord, OperationError> {
        let connection = self.lock_connection()?;
        let mut record = load_record(&connection, operation)?.ok_or(OperationError::NotFound)?;
        drop(connection);
        if let Some(error) = self.lock_errors()?.get(&operation).cloned() {
            record.error = Some(error);
        }
        Ok(record)
    }

    /// Returns bounded nonterminal operation counts.
    ///
    /// # Errors
    ///
    /// Returns a typed SQLite or integer-conversion failure.
    pub fn counts(&self) -> Result<OperationCounts, OperationError> {
        let connection = self.lock_connection()?;
        let (queued, running, cancelling): (i64, i64, i64) = connection
            .query_row(
                "SELECT
                    COALESCE(SUM(CASE WHEN state = 'queued' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN state = 'running' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN state = 'cancelling' THEN 1 ELSE 0 END), 0)
                 FROM operations",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(map_sqlite_error)?;
        Ok(OperationCounts {
            queued: nonnegative_i64_to_u32(queued)?,
            running: nonnegative_i64_to_u32(running)?,
            cancelling: nonnegative_i64_to_u32(cancelling)?,
        })
    }

    /// Returns the number of durable nonterminal operations.
    ///
    /// # Errors
    ///
    /// Returns a typed SQLite or integer-conversion failure.
    pub fn active_count(&self) -> Result<u32, OperationError> {
        Ok(self.counts()?.active())
    }

    /// Revalidates catalog identity, schema, connection policy, and page integrity.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError`] when the catalog is foreign, tampered, corrupt,
    /// or no longer uses the configured defensive SQLite policy.
    pub fn quick_check(&self) -> Result<(), OperationError> {
        let connection = self.lock_connection()?;
        let storage = catalog_storage(&connection)?;
        validate_catalog_identity(&connection)?;
        validate_schema(&connection, storage)
    }

    /// Revalidates a persistent catalog through a separate read-only connection.
    ///
    /// This path never acquires the journal's writer mutex, so bounded diagnostics
    /// cannot block status, cancellation, or durable completion on that lock.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError`] for open, policy, identity, schema, or integrity failures.
    pub fn quick_check_path(path: &Path) -> Result<(), OperationError> {
        Self::quick_check_path_with_timeout(path, CATALOG_BUSY_TIMEOUT)
    }

    /// Revalidates a persistent catalog with a monotonic SQLite progress deadline.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError`] for open, timeout, policy, identity, schema, or
    /// integrity failures.
    pub fn quick_check_path_with_timeout(
        path: &Path,
        timeout: Duration,
    ) -> Result<(), OperationError> {
        if timeout.is_zero() {
            return Err(OperationError::DiagnosticTimedOut);
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(OperationError::DiagnosticTimedOut)?;
        Self::quick_check_path_until(path, deadline)
    }

    /// Revalidates a persistent catalog against an existing monotonic deadline.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError`] for an expired deadline, open, policy, identity,
    /// schema, or integrity failure.
    pub fn quick_check_path_until(path: &Path, deadline: Instant) -> Result<(), OperationError> {
        check_diagnostic_deadline(deadline)?;
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        let connection = Connection::open_with_flags(path, flags).map_err(map_sqlite_error)?;
        connection.busy_handler(None).map_err(map_sqlite_error)?;
        connection
            .progress_handler(1_000, Some(move || Instant::now() >= deadline))
            .map_err(map_sqlite_error)?;
        configure_read_only_diagnostic_connection(&connection)?;
        check_diagnostic_deadline(deadline)?;
        map_diagnostic_timeout(validate_catalog_identity(&connection))?;
        check_diagnostic_deadline(deadline)?;
        map_diagnostic_timeout(validate_schema(&connection, CatalogStorage::Persistent))?;
        check_diagnostic_deadline(deadline)
    }

    fn recover_nonterminal(&self, now_unix_ms: u64) -> Result<(), OperationError> {
        loop {
            let changed = self.recover_nonterminal_batch(now_unix_ms, 256)?;
            if changed == 0 {
                return Ok(());
            }
        }
    }

    fn recover_nonterminal_batch(
        &self,
        now_unix_ms: u64,
        max_records: usize,
    ) -> Result<u32, OperationError> {
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        let ids = select_operation_ids(
            &transaction,
            "state IN ('queued', 'running', 'cancelling')",
            max_records,
            &[],
        )?;
        let mut changed = 0_u32;
        for operation in ids {
            let record =
                load_record(&transaction, operation)?.ok_or(OperationError::CorruptState)?;
            let recovery = recovery_for(&record, now_unix_ms);
            changed = changed
                .checked_add(update_interrupted(&transaction, operation, recovery)?)
                .ok_or(OperationError::CorruptState)?;
        }
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(changed)
    }

    fn prune_to(&self, limit: usize) -> Result<(), OperationError> {
        let limit = i64::try_from(limit).map_err(|_| OperationError::CorruptState)?;
        let connection = self.lock_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT operation FROM operations
                 WHERE state IN ('succeeded', 'failed', 'cancelled', 'interrupted')
                 ORDER BY sequence DESC LIMIT -1 OFFSET ?1",
            )
            .map_err(map_sqlite_error)?;
        let pruned = statement
            .query_map([limit], |row| row.get::<_, Vec<u8>>(0))
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        drop(statement);
        connection
            .execute(
                "DELETE FROM operations
                 WHERE operation IN (
                    SELECT operation FROM operations
                    WHERE state IN ('succeeded', 'failed', 'cancelled', 'interrupted')
                    ORDER BY sequence DESC LIMIT -1 OFFSET ?1
                 )",
                [limit],
            )
            .map_err(map_sqlite_error)?;
        drop(connection);
        let mut errors = self.lock_errors()?;
        for bytes in pruned {
            if let Ok(array) = <[u8; 16]>::try_from(bytes) {
                errors.remove(&OperationId::from_bytes(array));
            }
        }
        Ok(())
    }

    fn lock_connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, OperationError> {
        self.connection
            .lock()
            .map_err(|_| OperationError::MutexPoisoned)
    }

    fn lock_cancellations(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<OperationId, Cancellation>>, OperationError>
    {
        self.cancellations
            .lock()
            .map_err(|_| OperationError::MutexPoisoned)
    }

    fn lock_errors(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<OperationId, PublicError>>, OperationError> {
        self.errors
            .lock()
            .map_err(|_| OperationError::MutexPoisoned)
    }
}

/// Exclusive operating-system lock for the writable catalog.
///
/// The private file may remain after shutdown. Ownership follows the file handle,
/// so process termination releases the lock without PID probing or stale deletion.
#[derive(Debug)]
pub struct CatalogWriterLock {
    file: File,
}

impl CatalogWriterLock {
    /// Acquires the exclusive catalog writer.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::WriterBusy`] while another process owns the lock.
    pub fn acquire(path: &Path, _nonce: [u8; 16]) -> Result<Self, OperationError> {
        let file = private_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(OperationError::WriterBusy),
            Err(TryLockError::Error(source)) => return Err(OperationError::LockIo(source)),
        }
        Ok(Self { file })
    }
}

impl Drop for CatalogWriterLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn configure_catalog_connection(
    connection: &Connection,
    storage: CatalogStorage,
) -> Result<(), OperationError> {
    connection
        .busy_timeout(CATALOG_BUSY_TIMEOUT)
        .map_err(map_sqlite_error)?;
    for (config, enabled) in [
        (DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true),
        (DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
        (DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
        (DbConfig::SQLITE_DBCONFIG_DQS_DDL, false),
        (DbConfig::SQLITE_DBCONFIG_DQS_DML, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_CREATE, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE, false),
    ] {
        let observed = connection
            .set_db_config(config, enabled)
            .map_err(map_sqlite_error)?;
        if observed != enabled {
            return Err(OperationError::UnsupportedSqliteConfiguration);
        }
    }
    let requested_mode = match storage {
        CatalogStorage::Persistent => "WAL",
        CatalogStorage::Memory => "MEMORY",
    };
    let journal_mode: String = connection
        .pragma_update_and_check(None, "journal_mode", requested_mode, |row| row.get(0))
        .map_err(map_sqlite_error)?;
    if !journal_mode.eq_ignore_ascii_case(storage.journal_mode()) {
        return Err(OperationError::UnsupportedSqliteConfiguration);
    }
    connection
        .execute_batch(
            "PRAGMA synchronous = FULL;
             PRAGMA wal_autocheckpoint = 256;
             PRAGMA temp_store = MEMORY;",
        )
        .map_err(map_sqlite_error)?;
    validate_catalog_connection(connection, storage)
}

fn remaining_diagnostic_time(deadline: Instant) -> Result<Duration, OperationError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(OperationError::DiagnosticTimedOut)
}

fn check_diagnostic_deadline(deadline: Instant) -> Result<(), OperationError> {
    remaining_diagnostic_time(deadline).map(|_| ())
}

fn map_diagnostic_timeout<T>(result: Result<T, OperationError>) -> Result<T, OperationError> {
    match result {
        Err(OperationError::Sqlite(error))
            if error.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted) =>
        {
            Err(OperationError::DiagnosticTimedOut)
        }
        result => result,
    }
}

fn configure_read_only_diagnostic_connection(
    connection: &Connection,
) -> Result<(), OperationError> {
    for (config, enabled) in [
        (DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true),
        (DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
        (DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
        (DbConfig::SQLITE_DBCONFIG_DQS_DDL, false),
        (DbConfig::SQLITE_DBCONFIG_DQS_DML, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_CREATE, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE, false),
    ] {
        let observed = connection
            .set_db_config(config, enabled)
            .map_err(map_sqlite_error)?;
        if observed != enabled {
            return Err(OperationError::UnsupportedSqliteConfiguration);
        }
    }
    connection
        .execute_batch("PRAGMA query_only = ON; PRAGMA temp_store = MEMORY;")
        .map_err(map_sqlite_error)?;
    install_catalog_authorizer(connection)?;
    Ok(())
}

fn catalog_storage(connection: &Connection) -> Result<CatalogStorage, OperationError> {
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    if journal_mode.eq_ignore_ascii_case(CatalogStorage::Persistent.journal_mode()) {
        Ok(CatalogStorage::Persistent)
    } else if journal_mode.eq_ignore_ascii_case(CatalogStorage::Memory.journal_mode()) {
        Ok(CatalogStorage::Memory)
    } else {
        Err(OperationError::UnsupportedSqliteConfiguration)
    }
}

fn validate_catalog_connection(
    connection: &Connection,
    storage: CatalogStorage,
) -> Result<(), OperationError> {
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    let synchronous: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    let foreign_keys: i64 = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    let trusted_schema: i64 = connection
        .query_row("PRAGMA trusted_schema", [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    if !journal_mode.eq_ignore_ascii_case(storage.journal_mode())
        || synchronous != 2
        || foreign_keys != 1
        || trusted_schema != 0
        || !connection
            .db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)
            .map_err(map_sqlite_error)?
        || connection
            .db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL)
            .map_err(map_sqlite_error)?
        || connection
            .db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML)
            .map_err(map_sqlite_error)?
        || connection
            .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_CREATE)
            .map_err(map_sqlite_error)?
        || connection
            .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE)
            .map_err(map_sqlite_error)?
    {
        return Err(OperationError::UnsupportedSqliteConfiguration);
    }
    Ok(())
}

fn install_catalog_authorizer(connection: &Connection) -> Result<(), OperationError> {
    connection
        .authorizer(Some(
            |context: rusqlite::hooks::AuthContext<'_>| match context.action {
                AuthAction::Attach { .. }
                | AuthAction::Detach { .. }
                | AuthAction::CreateTempIndex { .. }
                | AuthAction::CreateTempTable { .. }
                | AuthAction::CreateTempTrigger { .. }
                | AuthAction::CreateTempView { .. }
                | AuthAction::DropTempIndex { .. }
                | AuthAction::DropTempTable { .. }
                | AuthAction::DropTempTrigger { .. }
                | AuthAction::DropTempView { .. }
                | AuthAction::CreateVtable { .. }
                | AuthAction::DropVtable { .. } => Authorization::Deny,
                _ => Authorization::Allow,
            },
        ))
        .map_err(map_sqlite_error)
}

fn migrate_schema(
    connection: &mut Connection,
    storage: CatalogStorage,
) -> Result<(), OperationError> {
    let observed_application_id = pragma_u32(connection, "application_id")?;
    let observed = pragma_u32(connection, "user_version")?;
    let has_rootlight_tables = table_exists(connection, "operations")?
        || table_exists(connection, "application_meta")?
        || table_exists(connection, "migrations")?;
    if observed_application_id != 0 && observed_application_id != CATALOG_APPLICATION_ID {
        return Err(OperationError::ForeignCatalog);
    }
    if observed_application_id == 0
        && !has_rootlight_tables
        && database_has_user_tables(connection)?
    {
        return Err(OperationError::ForeignCatalog);
    }
    if observed > OPERATION_SCHEMA_VERSION {
        return Err(OperationError::UnsupportedSchemaVersion { observed });
    }
    if observed == OPERATION_SCHEMA_VERSION {
        validate_schema(connection, storage)?;
        return Ok(());
    }

    let transaction = connection.transaction().map_err(map_sqlite_error)?;
    match observed {
        0 if !table_exists(&transaction, "operations")? => create_schema(&transaction)?,
        0 if is_prototype_schema(&transaction)? => migrate_prototype(&transaction)?,
        1 if is_version_one_operations_schema(&transaction)? => migrate_version_one(&transaction)?,
        2 if is_version_two_operations_schema(&transaction)? => {
            validate_version_two_catalog_identity(&transaction)?;
            migrate_version_two(&transaction)?;
        }
        _ => return Err(OperationError::UnsupportedLegacySchema),
    }
    record_catalog_metadata(&transaction)?;
    transaction
        .pragma_update(None, "application_id", CATALOG_APPLICATION_ID)
        .map_err(map_sqlite_error)?;
    transaction
        .pragma_update(None, "user_version", OPERATION_SCHEMA_VERSION)
        .map_err(map_sqlite_error)?;
    transaction.commit().map_err(map_sqlite_error)?;
    validate_schema(connection, storage)
}

const APPLICATION_META_SCHEMA_SQL: &str = "CREATE TABLE application_meta (
                key TEXT PRIMARY KEY NOT NULL,
                value BLOB NOT NULL
            ) STRICT";
const MIGRATIONS_SCHEMA_SQL: &str = "CREATE TABLE migrations (
                migration_id INTEGER PRIMARY KEY NOT NULL,
                checksum BLOB NOT NULL CHECK(length(checksum) = 32)
            ) STRICT";
const VERSION_ONE_OPERATIONS_SCHEMA_SQL: &str = "CREATE TABLE operations (
                operation BLOB PRIMARY KEY NOT NULL CHECK(length(operation) = 16),
                kind TEXT NOT NULL CHECK(kind IN ('control_probe')),
                plan_hash BLOB NOT NULL CHECK(length(plan_hash) = 32),
                owner BLOB NOT NULL CHECK(length(owner) = 16),
                detached INTEGER NOT NULL CHECK(detached IN (0, 1)),
                deadline_unix_ms INTEGER CHECK(deadline_unix_ms > 0),
                lease_expires_unix_ms INTEGER CHECK(lease_expires_unix_ms > 0),
                state TEXT NOT NULL CHECK(state IN (
                    'queued', 'running', 'cancelling', 'succeeded', 'failed',
                    'cancelled', 'interrupted'
                )),
                stage TEXT NOT NULL CHECK(stage IN ('accepted', 'executing', 'cleanup')),
                cancellation_requested INTEGER NOT NULL CHECK(cancellation_requested IN (0, 1)),
                cancellation_reason TEXT CHECK(cancellation_reason IN (
                    'client_request', 'parent_cancelled', 'deadline_exceeded',
                    'shutdown', 'resource_limit'
                )),
                recovery_class TEXT NOT NULL CHECK(recovery_class IN (
                    'not_applicable', 'interrupted_by_restart', 'deadline_elapsed', 'lease_expired'
                )),
                revision INTEGER NOT NULL CHECK(revision >= 1),
                completed INTEGER NOT NULL CHECK(completed >= 0 AND completed <= 4294967295),
                total INTEGER NOT NULL CHECK(total >= 0 AND total <= 4294967295),
                error_json TEXT CHECK(length(error_json) <= 16384),
                sequence INTEGER NOT NULL UNIQUE CHECK(sequence >= 1),
                CHECK(total = 0 OR completed <= total),
                CHECK((detached = 1 AND lease_expires_unix_ms IS NULL)
                   OR (detached = 0 AND lease_expires_unix_ms IS NOT NULL)),
                CHECK((cancellation_requested = 0 AND cancellation_reason IS NULL)
                   OR (cancellation_requested = 1 AND cancellation_reason IS NOT NULL)),
                CHECK((state = 'failed' AND error_json IS NOT NULL)
                   OR (state != 'failed' AND error_json IS NULL)),
                CHECK((state = 'interrupted' AND recovery_class != 'not_applicable')
                   OR (state != 'interrupted' AND recovery_class = 'not_applicable'))
            )";
const VERSION_TWO_OPERATIONS_SCHEMA_SQL: &str = "CREATE TABLE operations (
                operation BLOB PRIMARY KEY NOT NULL CHECK(length(operation) = 16),
                kind TEXT NOT NULL CHECK(kind IN ('control_probe')),
                plan_hash BLOB NOT NULL CHECK(length(plan_hash) = 32),
                owner BLOB NOT NULL CHECK(length(owner) = 16),
                detached INTEGER NOT NULL CHECK(detached IN (0, 1)),
                deadline_unix_ms INTEGER CHECK(deadline_unix_ms > 0),
                lease_expires_unix_ms INTEGER CHECK(lease_expires_unix_ms > 0),
                state TEXT NOT NULL CHECK(state IN (
                    'queued', 'running', 'cancelling', 'succeeded', 'failed',
                    'cancelled', 'interrupted'
                )),
                stage TEXT NOT NULL CHECK(stage IN ('accepted', 'executing', 'cleanup')),
                cancellation_requested INTEGER NOT NULL CHECK(cancellation_requested IN (0, 1)),
                cancellation_reason TEXT CHECK(cancellation_reason IN (
                    'client_request', 'parent_cancelled', 'deadline_exceeded',
                    'shutdown', 'resource_limit'
                )),
                recovery_class TEXT NOT NULL CHECK(recovery_class IN (
                    'not_applicable', 'interrupted_by_restart', 'deadline_elapsed', 'lease_expired'
                )),
                revision INTEGER NOT NULL CHECK(revision >= 1),
                completed INTEGER NOT NULL CHECK(completed >= 0 AND completed <= 4294967295),
                total INTEGER NOT NULL CHECK(total >= 0 AND total <= 4294967295),
                error_json TEXT CHECK(length(error_json) <= 16384),
                sequence INTEGER NOT NULL UNIQUE CHECK(sequence >= 1),
                CHECK(total = 0 OR completed <= total),
                CHECK((detached = 1 AND lease_expires_unix_ms IS NULL)
                   OR (detached = 0 AND lease_expires_unix_ms IS NOT NULL)),
                CHECK((cancellation_requested = 0 AND cancellation_reason IS NULL)
                   OR (cancellation_requested = 1 AND cancellation_reason IS NOT NULL)),
                CHECK((state = 'failed' AND error_json IS NOT NULL)
                   OR (state != 'failed' AND error_json IS NULL)),
                CHECK((state = 'interrupted' AND recovery_class != 'not_applicable')
                   OR (state != 'interrupted' AND recovery_class = 'not_applicable'))
            ) STRICT";
const OPERATIONS_SCHEMA_SQL: &str = "CREATE TABLE operations (
                operation BLOB PRIMARY KEY NOT NULL CHECK(length(operation) = 16),
                kind TEXT NOT NULL CHECK(kind IN ('control_probe')),
                plan_hash BLOB NOT NULL CHECK(length(plan_hash) = 32),
                owner BLOB NOT NULL CHECK(length(owner) = 16),
                detached INTEGER NOT NULL CHECK(detached IN (0, 1)),
                deadline_unix_ms INTEGER CHECK(deadline_unix_ms > 0),
                relative_timeout_ms INTEGER CHECK(relative_timeout_ms > 0),
                lease_expires_unix_ms INTEGER CHECK(lease_expires_unix_ms > 0),
                state TEXT NOT NULL CHECK(state IN (
                    'queued', 'running', 'cancelling', 'succeeded', 'failed',
                    'cancelled', 'interrupted'
                )),
                stage TEXT NOT NULL CHECK(stage IN ('accepted', 'executing', 'cleanup')),
                cancellation_requested INTEGER NOT NULL CHECK(cancellation_requested IN (0, 1)),
                cancellation_reason TEXT CHECK(cancellation_reason IN (
                    'client_request', 'parent_cancelled', 'deadline_exceeded',
                    'shutdown', 'resource_limit'
                )),
                recovery_class TEXT NOT NULL CHECK(recovery_class IN (
                    'not_applicable', 'interrupted_by_restart', 'deadline_elapsed', 'lease_expired'
                )),
                revision INTEGER NOT NULL CHECK(revision >= 1),
                completed INTEGER NOT NULL CHECK(completed >= 0 AND completed <= 4294967295),
                total INTEGER NOT NULL CHECK(total >= 0 AND total <= 4294967295),
                error_json TEXT CHECK(length(error_json) <= 16384),
                sequence INTEGER NOT NULL UNIQUE CHECK(sequence >= 1),
                CHECK(total = 0 OR completed <= total),
                CHECK((detached = 1 AND lease_expires_unix_ms IS NULL)
                   OR (detached = 0 AND lease_expires_unix_ms IS NOT NULL)),
                CHECK(relative_timeout_ms IS NULL
                   OR (detached = 1 AND deadline_unix_ms IS NOT NULL)),
                CHECK((cancellation_requested = 0 AND cancellation_reason IS NULL)
                   OR (cancellation_requested = 1 AND cancellation_reason IS NOT NULL)),
                CHECK((state = 'failed' AND error_json IS NOT NULL)
                   OR (state != 'failed' AND error_json IS NULL)),
                CHECK((state = 'interrupted' AND recovery_class != 'not_applicable')
                   OR (state != 'interrupted' AND recovery_class = 'not_applicable'))
            ) STRICT";

fn migration_checksum_input() -> String {
    [
        APPLICATION_META_SCHEMA_SQL,
        MIGRATIONS_SCHEMA_SQL,
        OPERATIONS_SCHEMA_SQL,
    ]
    .join("\n")
}

fn operation_schema_migration_checksum() -> [u8; 32] {
    Sha256::digest(migration_checksum_input()).into()
}

fn create_schema(connection: &Connection) -> Result<(), OperationError> {
    for statement in [
        APPLICATION_META_SCHEMA_SQL,
        MIGRATIONS_SCHEMA_SQL,
        OPERATIONS_SCHEMA_SQL,
    ] {
        connection
            .execute_batch(statement)
            .map_err(map_sqlite_error)?;
    }
    Ok(())
}

fn migrate_prototype(transaction: &Transaction<'_>) -> Result<(), OperationError> {
    transaction
        .execute_batch("ALTER TABLE operations RENAME TO operations_v0;")
        .map_err(map_sqlite_error)?;
    create_schema(transaction)?;
    transaction
        .execute(
            "INSERT INTO operations (
                operation, kind, plan_hash, owner, detached, deadline_unix_ms,
                relative_timeout_ms, lease_expires_unix_ms, state, stage, cancellation_requested,
                cancellation_reason, recovery_class, revision, completed, total,
                error_json, sequence
             ) SELECT operation, 'control_probe', zeroblob(32), zeroblob(16), 1,
                      NULL, NULL, NULL, state, 'accepted',
                      CASE WHEN state = 'cancelling' THEN 1 ELSE 0 END,
                      CASE WHEN state = 'cancelling' THEN 'client_request' ELSE NULL END,
                      CASE WHEN state = 'interrupted' THEN 'interrupted_by_restart'
                           ELSE 'not_applicable' END,
                      revision, completed, total, error_json, sequence
               FROM operations_v0",
            [],
        )
        .map_err(map_sqlite_error)?;
    transaction
        .execute_batch("DROP TABLE operations_v0;")
        .map_err(map_sqlite_error)
}

fn migrate_version_one(transaction: &Transaction<'_>) -> Result<(), OperationError> {
    transaction
        .execute_batch("ALTER TABLE operations RENAME TO operations_v1;")
        .map_err(map_sqlite_error)?;
    create_schema(transaction)?;
    transaction
        .execute_batch(
            "INSERT INTO operations (
                operation, kind, plan_hash, owner, detached, deadline_unix_ms,
                relative_timeout_ms, lease_expires_unix_ms, state, stage, cancellation_requested,
                cancellation_reason, recovery_class, revision, completed, total,
                error_json, sequence
             ) SELECT operation, kind, plan_hash, owner, detached, deadline_unix_ms,
                      NULL, lease_expires_unix_ms, state, stage, cancellation_requested,
                      cancellation_reason, recovery_class, revision, completed, total,
                      error_json, sequence
               FROM operations_v1;
             DROP TABLE operations_v1;",
        )
        .map_err(map_sqlite_error)
}

fn migrate_version_two(transaction: &Transaction<'_>) -> Result<(), OperationError> {
    transaction
        .execute_batch("ALTER TABLE operations RENAME TO operations_v2;")
        .map_err(map_sqlite_error)?;
    transaction
        .execute_batch(OPERATIONS_SCHEMA_SQL)
        .map_err(map_sqlite_error)?;
    // Version two stored only the derived wall deadline. Leaving the intent
    // absent rejects relative retries instead of guessing their old timeout.
    transaction
        .execute_batch(
            "INSERT INTO operations (
                operation, kind, plan_hash, owner, detached, deadline_unix_ms,
                relative_timeout_ms, lease_expires_unix_ms, state, stage,
                cancellation_requested, cancellation_reason, recovery_class, revision,
                completed, total, error_json, sequence
             ) SELECT operation, kind, plan_hash, owner, detached, deadline_unix_ms,
                      NULL, lease_expires_unix_ms, state, stage, cancellation_requested,
                      cancellation_reason, recovery_class, revision, completed, total,
                      error_json, sequence
               FROM operations_v2;
             DROP TABLE operations_v2;",
        )
        .map_err(map_sqlite_error)
}

fn record_catalog_metadata(connection: &Connection) -> Result<(), OperationError> {
    let expected_checksum = operation_schema_migration_checksum();
    if expected_checksum != OPERATION_SCHEMA_MIGRATION_CHECKSUM {
        return Err(OperationError::MigrationChecksumMismatch);
    }
    let existing_checksum: Option<Vec<u8>> = connection
        .query_row(
            "SELECT checksum FROM migrations WHERE migration_id = ?1",
            [OPERATION_SCHEMA_MIGRATION_ID],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite_error)?;
    if existing_checksum
        .as_deref()
        .is_some_and(|checksum| checksum != OPERATION_SCHEMA_MIGRATION_CHECKSUM)
    {
        return Err(OperationError::MigrationChecksumMismatch);
    }
    connection
        .execute(
            "INSERT INTO application_meta(key, value) VALUES ('catalog_kind', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [b"rootlight".as_slice()],
        )
        .map_err(map_sqlite_error)?;
    connection
        .execute(
            "INSERT INTO application_meta(key, value) VALUES ('sqlite_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [rusqlite::version().as_bytes()],
        )
        .map_err(map_sqlite_error)?;
    let compile_options = sqlite_compile_options(connection)?.join("\n");
    connection
        .execute(
            "INSERT INTO application_meta(key, value) VALUES ('sqlite_compile_options', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [compile_options.as_bytes()],
        )
        .map_err(map_sqlite_error)?;
    connection
        .execute(
            "INSERT INTO migrations(migration_id, checksum) VALUES (?1, ?2)
             ON CONFLICT(migration_id) DO NOTHING",
            params![
                OPERATION_SCHEMA_MIGRATION_ID,
                OPERATION_SCHEMA_MIGRATION_CHECKSUM.as_slice(),
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn validate_catalog_identity(connection: &Connection) -> Result<(), OperationError> {
    if operation_schema_migration_checksum() != OPERATION_SCHEMA_MIGRATION_CHECKSUM {
        return Err(OperationError::MigrationChecksumMismatch);
    }
    if pragma_u32(connection, "application_id")? != CATALOG_APPLICATION_ID
        || pragma_u32(connection, "user_version")? != OPERATION_SCHEMA_VERSION
    {
        return Err(OperationError::ForeignCatalog);
    }
    let kind: Option<Vec<u8>> = connection
        .query_row(
            "SELECT value FROM application_meta WHERE key = 'catalog_kind'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite_error)?;
    if kind.as_deref() != Some(b"rootlight".as_slice()) {
        return Err(OperationError::ForeignCatalog);
    }
    let checksum: Option<Vec<u8>> = connection
        .query_row(
            "SELECT checksum FROM migrations WHERE migration_id = ?1",
            [OPERATION_SCHEMA_MIGRATION_ID],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite_error)?;
    if checksum.as_deref() != Some(OPERATION_SCHEMA_MIGRATION_CHECKSUM.as_slice()) {
        return Err(OperationError::MigrationChecksumMismatch);
    }
    Ok(())
}

fn validate_version_two_catalog_identity(connection: &Connection) -> Result<(), OperationError> {
    if pragma_u32(connection, "application_id")? != CATALOG_APPLICATION_ID {
        return Err(OperationError::ForeignCatalog);
    }
    let kind: Option<Vec<u8>> = connection
        .query_row(
            "SELECT value FROM application_meta WHERE key = 'catalog_kind'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite_error)?;
    if kind.as_deref() != Some(b"rootlight".as_slice()) {
        return Err(OperationError::ForeignCatalog);
    }
    let checksum: Option<Vec<u8>> = connection
        .query_row(
            "SELECT checksum FROM migrations WHERE migration_id = ?1",
            [VERSION_TWO_SCHEMA_MIGRATION_ID],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite_error)?;
    if checksum.as_deref() != Some(VERSION_TWO_SCHEMA_MIGRATION_CHECKSUM.as_slice()) {
        return Err(OperationError::MigrationChecksumMismatch);
    }
    Ok(())
}

fn validate_schema(connection: &Connection, storage: CatalogStorage) -> Result<(), OperationError> {
    let columns = table_columns(connection)?;
    let expected = [
        "operation",
        "kind",
        "plan_hash",
        "owner",
        "detached",
        "deadline_unix_ms",
        "relative_timeout_ms",
        "lease_expires_unix_ms",
        "state",
        "stage",
        "cancellation_requested",
        "cancellation_reason",
        "recovery_class",
        "revision",
        "completed",
        "total",
        "error_json",
        "sequence",
    ];
    if columns != expected
        || table_columns_named(connection, "application_meta")? != ["key", "value"]
        || table_columns_named(connection, "migrations")? != ["migration_id", "checksum"]
        || !table_is_strict(connection, "application_meta")?
        || !table_is_strict(connection, "migrations")?
        || !table_is_strict(connection, "operations")?
        || normalize_sql(&table_definition(connection, "application_meta")?)
            != normalize_sql(APPLICATION_META_SCHEMA_SQL)
        || normalize_sql(&table_definition(connection, "migrations")?)
            != normalize_sql(MIGRATIONS_SCHEMA_SQL)
        || normalize_sql(&table_definition(connection, "operations")?)
            != normalize_sql(OPERATIONS_SCHEMA_SQL)
    {
        return Err(OperationError::CorruptSchema);
    }
    validate_catalog_connection(connection, storage)?;
    connection
        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
        .map_err(map_sqlite_error)
        .and_then(|result| {
            if result == "ok" {
                Ok(())
            } else {
                Err(OperationError::CorruptSchema)
            }
        })
}

fn pragma_u32(connection: &Connection, pragma: &str) -> Result<u32, OperationError> {
    let sql = match pragma {
        "application_id" => "PRAGMA application_id",
        "user_version" => "PRAGMA user_version",
        _ => return Err(OperationError::CorruptState),
    };
    let observed: i64 = connection
        .query_row(sql, [], |row| row.get(0))
        .map_err(map_sqlite_error)?;
    u32::try_from(observed).map_err(|_| OperationError::CorruptState)
}

fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn table_definition(connection: &Connection, table: &str) -> Result<String, OperationError> {
    if !matches!(table, "operations" | "application_meta" | "migrations") {
        return Err(OperationError::CorruptState);
    }
    connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, String>(0),
        )
        .map(|definition| definition.trim().trim_end_matches(';').to_owned())
        .map_err(map_sqlite_error)
}

fn table_is_strict(connection: &Connection, table: &str) -> Result<bool, OperationError> {
    if !matches!(table, "operations" | "application_meta" | "migrations") {
        return Err(OperationError::CorruptState);
    }
    connection
        .query_row(
            "SELECT strict FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .map(|strict| strict == 1)
        .map_err(map_sqlite_error)
}

fn database_has_user_tables(connection: &Connection) -> Result<bool, OperationError> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(map_sqlite_error)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, OperationError> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(map_sqlite_error)
}

fn is_prototype_schema(connection: &Connection) -> Result<bool, OperationError> {
    Ok(table_columns(connection)?
        == [
            "operation",
            "state",
            "revision",
            "completed",
            "total",
            "error_json",
            "sequence",
        ])
}

fn is_version_one_operations_schema(connection: &Connection) -> Result<bool, OperationError> {
    Ok(table_columns(connection)?
        == [
            "operation",
            "kind",
            "plan_hash",
            "owner",
            "detached",
            "deadline_unix_ms",
            "lease_expires_unix_ms",
            "state",
            "stage",
            "cancellation_requested",
            "cancellation_reason",
            "recovery_class",
            "revision",
            "completed",
            "total",
            "error_json",
            "sequence",
        ]
        && !table_is_strict(connection, "operations")?
        && normalize_sql(&table_definition(connection, "operations")?)
            == normalize_sql(VERSION_ONE_OPERATIONS_SCHEMA_SQL))
}

fn is_version_two_operations_schema(connection: &Connection) -> Result<bool, OperationError> {
    Ok(table_columns(connection)?
        == [
            "operation",
            "kind",
            "plan_hash",
            "owner",
            "detached",
            "deadline_unix_ms",
            "lease_expires_unix_ms",
            "state",
            "stage",
            "cancellation_requested",
            "cancellation_reason",
            "recovery_class",
            "revision",
            "completed",
            "total",
            "error_json",
            "sequence",
        ]
        && table_is_strict(connection, "operations")?
        && normalize_sql(&table_definition(connection, "operations")?)
            == normalize_sql(VERSION_TWO_OPERATIONS_SCHEMA_SQL))
}

fn table_columns(connection: &Connection) -> Result<Vec<String>, OperationError> {
    table_columns_named(connection, "operations")
}

fn table_columns_named(
    connection: &Connection,
    table: &str,
) -> Result<Vec<String>, OperationError> {
    if !matches!(table, "operations" | "application_meta" | "migrations") {
        return Err(OperationError::CorruptState);
    }
    let query = format!("PRAGMA table_info({table})");
    connection
        .prepare(&query)
        .map_err(map_sqlite_error)?
        .query_map([], |row| row.get(1))
        .map_err(map_sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(map_sqlite_error)
}

fn load_record(
    connection: &Connection,
    operation: OperationId,
) -> Result<Option<OperationRecord>, OperationError> {
    Ok(load_record_with_retry_intent(connection, operation)?
        .map(|(record, _relative_timeout_ms)| record))
}

fn load_record_with_retry_intent(
    connection: &Connection,
    operation: OperationId,
) -> Result<Option<(OperationRecord, Option<u64>)>, OperationError> {
    let raw = connection
        .query_row(
            "SELECT kind, plan_hash, owner, detached, deadline_unix_ms,
                    relative_timeout_ms, lease_expires_unix_ms, state, stage, cancellation_requested,
                    cancellation_reason, recovery_class, revision, completed, total,
                    error_json
             FROM operations WHERE operation = ?1",
            [operation.as_bytes().as_slice()],
            |row| {
                Ok(RawRecord {
                    kind: row.get(0)?,
                    plan_hash: row.get(1)?,
                    owner: row.get(2)?,
                    detached: row.get(3)?,
                    deadline_unix_ms: row.get(4)?,
                    relative_timeout_ms: row.get(5)?,
                    lease_expires_unix_ms: row.get(6)?,
                    state: row.get(7)?,
                    stage: row.get(8)?,
                    cancellation_requested: row.get(9)?,
                    cancellation_reason: row.get(10)?,
                    recovery_class: row.get(11)?,
                    revision: row.get(12)?,
                    completed: row.get(13)?,
                    total: row.get(14)?,
                    error_json: row.get(15)?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite_error)?;
    raw.map(|raw| {
        let relative_timeout_ms = optional_nonnegative_i64_to_u64(raw.relative_timeout_ms)?;
        if relative_timeout_ms == Some(0)
            || relative_timeout_ms.is_some()
                && (raw.detached != 1 || raw.deadline_unix_ms.is_none())
        {
            return Err(OperationError::CorruptState);
        }
        decode_record(operation, raw).map(|record| (record, relative_timeout_ms))
    })
    .transpose()
}

struct RawRecord {
    kind: String,
    plan_hash: Vec<u8>,
    owner: Vec<u8>,
    detached: i64,
    deadline_unix_ms: Option<i64>,
    relative_timeout_ms: Option<i64>,
    lease_expires_unix_ms: Option<i64>,
    state: String,
    stage: String,
    cancellation_requested: i64,
    cancellation_reason: Option<String>,
    recovery_class: String,
    revision: i64,
    completed: i64,
    total: i64,
    error_json: Option<String>,
}

fn decode_record(
    operation: OperationId,
    raw: RawRecord,
) -> Result<OperationRecord, OperationError> {
    let plan_hash = PlanHash::from_bytes(
        raw.plan_hash
            .try_into()
            .map_err(|_| OperationError::CorruptState)?,
    );
    let owner = ClientInstanceId::from_bytes(
        raw.owner
            .try_into()
            .map_err(|_| OperationError::CorruptState)?,
    );
    let detached = i64_to_bool(raw.detached)?;
    let cancellation_requested = i64_to_bool(raw.cancellation_requested)?;
    let cancellation_reason = raw
        .cancellation_reason
        .as_deref()
        .map(parse_cancellation_reason)
        .transpose()?;
    if cancellation_requested != cancellation_reason.is_some() {
        return Err(OperationError::CorruptState);
    }
    let state = OperationState::parse(&raw.state)?;
    let recovery_class = RecoveryClass::parse(&raw.recovery_class)?;
    if (state == OperationState::Interrupted) == (recovery_class == RecoveryClass::NotApplicable) {
        return Err(OperationError::CorruptState);
    }
    let deadline_unix_ms = optional_nonnegative_i64_to_u64(raw.deadline_unix_ms)?;
    let lease_expires_unix_ms = optional_nonnegative_i64_to_u64(raw.lease_expires_unix_ms)?;
    if detached == lease_expires_unix_ms.is_some() {
        return Err(OperationError::CorruptState);
    }
    let has_persisted_error = raw.error_json.is_some();
    let error = raw
        .error_json
        .as_deref()
        .map(deserialize_public_error)
        .transpose()?;
    if (state == OperationState::Failed) != error.is_some() {
        return Err(OperationError::CorruptState);
    }
    Ok(OperationRecord {
        operation,
        kind: OperationKind::parse(&raw.kind)?,
        plan_hash,
        owner,
        detached,
        deadline_unix_ms,
        lease_expires_unix_ms,
        state,
        stage: OperationStage::parse(&raw.stage)?,
        cancellation_requested,
        cancellation_reason,
        recovery_class,
        revision: nonnegative_i64_to_u64(raw.revision)?,
        progress: Progress::new(
            nonnegative_i64_to_u32(raw.completed)?,
            nonnegative_i64_to_u32(raw.total)?,
        )?,
        error,
        has_persisted_error,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingInterruption {
    operation: OperationId,
    reason: CancellationReason,
    lease_expires_unix_ms: Option<u64>,
}

fn select_operation_ids(
    connection: &Connection,
    predicate: &str,
    max_records: usize,
    values: &[i64],
) -> Result<Vec<OperationId>, OperationError> {
    let limit = i64::try_from(max_records).map_err(|_| OperationError::CorruptState)?;
    let sql = format!(
        "SELECT operation FROM operations WHERE {predicate} ORDER BY sequence ASC LIMIT ?{}",
        values.len() + 1
    );
    let mut parameters: Vec<&dyn rusqlite::ToSql> = values
        .iter()
        .map(|value| value as &dyn rusqlite::ToSql)
        .collect();
    parameters.push(&limit);
    connection
        .prepare(&sql)
        .map_err(map_sqlite_error)?
        .query_map(parameters.as_slice(), |row| row.get::<_, Vec<u8>>(0))
        .map_err(map_sqlite_error)?
        .map(|value| {
            value
                .map_err(map_sqlite_error)
                .and_then(|bytes| {
                    <[u8; 16]>::try_from(bytes).map_err(|_| OperationError::CorruptState)
                })
                .map(OperationId::from_bytes)
        })
        .collect()
}

fn update_interrupted(
    transaction: &Transaction<'_>,
    operation: OperationId,
    recovery: RecoveryClass,
) -> Result<u32, OperationError> {
    let updated = transaction
        .execute(
            "UPDATE operations
             SET state = 'interrupted', recovery_class = ?1, revision = revision + 1
             WHERE operation = ?2 AND state IN ('queued', 'running', 'cancelling')",
            params![recovery.as_str(), operation.as_bytes().as_slice()],
        )
        .map_err(map_sqlite_error)?;
    u32::try_from(updated).map_err(|_| OperationError::CorruptState)
}

fn recovery_for(record: &OperationRecord, now_unix_ms: u64) -> RecoveryClass {
    if record
        .deadline_unix_ms
        .is_some_and(|deadline| deadline <= now_unix_ms)
    {
        RecoveryClass::DeadlineElapsed
    } else if !record.detached
        && record
            .lease_expires_unix_ms
            .is_some_and(|lease| lease <= now_unix_ms)
    {
        RecoveryClass::LeaseExpired
    } else {
        RecoveryClass::InterruptedByRestart
    }
}

fn submission_matches(
    record: &OperationRecord,
    stored_timeout_ms: Option<u64>,
    submission: OperationSubmission,
    deadline_retry: DeadlineRetry,
) -> bool {
    let owner_matches = record.owner == submission.owner || record.detached && submission.detached;
    record.operation == submission.operation
        && record.kind == submission.kind
        && record.plan_hash == submission.plan_hash
        && owner_matches
        && record.detached == submission.detached
        && deadline_matches(record, stored_timeout_ms, submission, deadline_retry)
        && record.lease_expires_unix_ms == submission.lease_expires_unix_ms
}

fn deadline_matches(
    record: &OperationRecord,
    stored_timeout_ms: Option<u64>,
    submission: OperationSubmission,
    deadline_retry: DeadlineRetry,
) -> bool {
    match (stored_timeout_ms, deadline_retry) {
        (None, DeadlineRetry::Exact) => record.deadline_unix_ms == submission.deadline_unix_ms,
        (Some(stored_timeout_ms), DeadlineRetry::ReanchoredRelative { timeout_ms })
            if record.detached && submission.detached =>
        {
            stored_timeout_ms == timeout_ms
                && record.deadline_unix_ms.is_some()
                && submission.deadline_unix_ms.is_some()
        }
        _ => false,
    }
}

fn validate_submission_with_retry(
    submission: OperationSubmission,
    deadline_retry: DeadlineRetry,
) -> Result<(), OperationError> {
    validate_submission(submission)?;
    match deadline_retry {
        DeadlineRetry::Exact => Ok(()),
        DeadlineRetry::ReanchoredRelative { timeout_ms }
            if timeout_ms > 0 && submission.detached && submission.deadline_unix_ms.is_some() =>
        {
            Ok(())
        }
        DeadlineRetry::ReanchoredRelative { .. } => Err(OperationError::InvalidSubmission),
    }
}

fn validate_submission(submission: OperationSubmission) -> Result<(), OperationError> {
    if submission.deadline_unix_ms == Some(0)
        || submission.lease_expires_unix_ms == Some(0)
        || submission.detached == submission.lease_expires_unix_ms.is_some()
    {
        return Err(OperationError::InvalidSubmission);
    }
    Ok(())
}

fn serialize_public_error(error: &PublicError) -> Result<String, OperationError> {
    let encoded = serde_json::to_string(error).map_err(OperationError::SerializePublicError)?;
    if encoded.len() > MAX_PUBLIC_ERROR_BYTES {
        return Err(OperationError::PublicErrorTooLarge);
    }
    Ok(encoded)
}

fn deserialize_public_error(encoded: &str) -> Result<PublicError, OperationError> {
    if encoded.len() > MAX_PUBLIC_ERROR_BYTES {
        return Err(OperationError::PublicErrorTooLarge);
    }
    serde_json::from_str(encoded).map_err(OperationError::DeserializePublicError)
}

fn cancellation_reason_as_str(reason: CancellationReason) -> Result<&'static str, OperationError> {
    match reason {
        CancellationReason::ClientRequest => Ok("client_request"),
        CancellationReason::ParentCancelled => Ok("parent_cancelled"),
        CancellationReason::DeadlineExceeded => Ok("deadline_exceeded"),
        CancellationReason::Shutdown => Ok("shutdown"),
        CancellationReason::ResourceLimit => Ok("resource_limit"),
        _ => Err(OperationError::UnsupportedCancellationReason),
    }
}

fn parse_cancellation_reason(value: &str) -> Result<CancellationReason, OperationError> {
    match value {
        "client_request" => Ok(CancellationReason::ClientRequest),
        "parent_cancelled" => Ok(CancellationReason::ParentCancelled),
        "deadline_exceeded" => Ok(CancellationReason::DeadlineExceeded),
        "shutdown" => Ok(CancellationReason::Shutdown),
        "resource_limit" => Ok(CancellationReason::ResourceLimit),
        _ => Err(OperationError::CorruptState),
    }
}

fn legal_transition(from: OperationState, to: OperationState) -> bool {
    matches!(
        (from, to),
        (OperationState::Queued, OperationState::Running)
            | (OperationState::Queued, OperationState::Failed)
            | (OperationState::Queued, OperationState::Interrupted)
            | (OperationState::Running, OperationState::Cancelling)
            | (OperationState::Running, OperationState::Succeeded)
            | (OperationState::Running, OperationState::Failed)
            | (OperationState::Running, OperationState::Interrupted)
            | (OperationState::Cancelling, OperationState::Cancelled)
            | (OperationState::Cancelling, OperationState::Failed)
            | (OperationState::Cancelling, OperationState::Interrupted)
    )
}

fn verify_sqlite(connection: &Connection) -> Result<(), OperationError> {
    if rusqlite::version_number() < MIN_SQLITE_VERSION_NUMBER {
        return Err(OperationError::UnsupportedSqlite {
            observed: rusqlite::version_number(),
        });
    }
    let compile_options = sqlite_compile_options(connection)?;
    if compile_options
        .iter()
        .any(|option| option == "OMIT_FOREIGN_KEY")
    {
        return Err(OperationError::UnsupportedSqliteCompileOptions);
    }
    Ok(())
}

fn sqlite_compile_options(connection: &Connection) -> Result<Vec<String>, OperationError> {
    connection
        .prepare("PRAGMA compile_options")
        .map_err(map_sqlite_error)?
        .query_map([], |row| row.get(0))
        .map_err(map_sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(map_sqlite_error)
}

fn map_sqlite_error(error: rusqlite::Error) -> OperationError {
    match &error {
        rusqlite::Error::SqliteFailure(source, _)
            if matches!(
                source.code,
                rusqlite::ffi::ErrorCode::DatabaseBusy | rusqlite::ffi::ErrorCode::DatabaseLocked
            ) =>
        {
            OperationError::Busy
        }
        _ => OperationError::Sqlite(error),
    }
}

fn next_sequence(connection: &Connection) -> Result<i64, OperationError> {
    connection
        .query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM operations",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_error)
}

fn next_revision(revision: u64) -> Result<u64, OperationError> {
    revision
        .checked_add(1)
        .ok_or(OperationError::RevisionOverflow)
}

fn unix_time_ms() -> Result<u64, OperationError> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| OperationError::SystemClockBeforeEpoch)?;
    u64::try_from(duration.as_millis()).map_err(|_| OperationError::TimestampOverflow)
}

fn bool_to_i64(value: bool) -> i64 {
    i64::from(value)
}

fn i64_to_bool(value: i64) -> Result<bool, OperationError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(OperationError::CorruptState),
    }
}

fn u64_to_i64(value: u64) -> Result<i64, OperationError> {
    i64::try_from(value).map_err(|_| OperationError::TimestampOverflow)
}

fn optional_u64_to_i64(value: Option<u64>) -> Result<Option<i64>, OperationError> {
    value.map(u64_to_i64).transpose()
}

fn nonnegative_i64_to_u64(value: i64) -> Result<u64, OperationError> {
    u64::try_from(value).map_err(|_| OperationError::CorruptState)
}

fn optional_nonnegative_i64_to_u64(value: Option<i64>) -> Result<Option<u64>, OperationError> {
    value.map(nonnegative_i64_to_u64).transpose()
}

fn nonnegative_i64_to_u32(value: i64) -> Result<u32, OperationError> {
    u32::try_from(value).map_err(|_| OperationError::CorruptState)
}

fn private_lock_file(path: &Path) -> Result<File, OperationError> {
    #[cfg(unix)]
    let file = {
        use nix::{
            fcntl::{OFlag, open},
            sys::stat::Mode,
        };

        let descriptor = open(
            path,
            OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|source| OperationError::LockIo(io::Error::from_raw_os_error(source as i32)))?;
        File::from(descriptor)
    };
    #[cfg(windows)]
    let file = {
        use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt as _};
        use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
        let file = options.open(path).map_err(OperationError::LockIo)?;
        validate_private_windows_lock_file(&file)?;
        apply_private_windows_lock_dacl(path)?;
        verify_private_windows_lock_dacl(path)?;
        file
    };
    #[cfg(unix)]
    validate_private_lock_file(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn validate_private_lock_file(file: &File) -> Result<(), OperationError> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata().map_err(OperationError::LockIo)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(OperationError::InsecureLockFile);
    }
    Ok(())
}

#[cfg(windows)]
fn validate_private_windows_lock_file(file: &File) -> Result<(), OperationError> {
    use std::os::windows::fs::MetadataExt as _;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    let metadata = file.metadata().map_err(OperationError::LockIo)?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    {
        return Err(OperationError::InsecureLockFile);
    }
    Ok(())
}

#[cfg(windows)]
fn apply_private_windows_lock_dacl(path: &Path) -> Result<(), OperationError> {
    use windows_permissions::{
        LocalBox, SecurityDescriptor,
        constants::{SeObjectType, SecurityInformation},
        wrappers::SetNamedSecurityInfo,
    };

    let sid = windows_account_sid()?;
    let descriptor: LocalBox<SecurityDescriptor> = format!("D:P(A;;FA;;;{sid})")
        .parse()
        .map_err(|_| OperationError::WindowsSecurityPolicy)?;
    let dacl = descriptor
        .dacl()
        .ok_or(OperationError::WindowsSecurityPolicy)?;
    SetNamedSecurityInfo(
        path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        None,
        None,
        Some(dacl),
        None,
    )
    .map_err(OperationError::LockIo)
}

#[cfg(windows)]
fn verify_private_windows_lock_dacl(path: &Path) -> Result<(), OperationError> {
    use windows_permissions::{
        constants::{AccessRights, AceType, SeObjectType, SecurityInformation},
        wrappers::{ConvertSecurityDescriptorToStringSecurityDescriptor, GetNamedSecurityInfo},
    };

    let expected_sid = windows_account_sid()?;
    let descriptor = GetNamedSecurityInfo(
        path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
    )
    .map_err(OperationError::LockIo)?;
    let dacl = descriptor
        .dacl()
        .ok_or(OperationError::WindowsSecurityPolicy)?;
    let sddl =
        ConvertSecurityDescriptorToStringSecurityDescriptor(&descriptor, SecurityInformation::Dacl)
            .map_err(OperationError::LockIo)?;
    if !sddl.to_string_lossy().starts_with("D:P") || dacl.len() != 1 {
        return Err(OperationError::WindowsSecurityPolicy);
    }
    let ace = dacl
        .get_ace(0)
        .ok_or(OperationError::WindowsSecurityPolicy)?;
    let observed_sid = ace
        .sid()
        .ok_or(OperationError::WindowsSecurityPolicy)?
        .to_string();
    if ace.ace_type() != AceType::ACCESS_ALLOWED_ACE_TYPE
        || ace.mask() != AccessRights::FileAllAccess
        || !ace.flags().is_empty()
        || observed_sid != expected_sid
    {
        return Err(OperationError::WindowsSecurityPolicy);
    }
    Ok(())
}

#[cfg(windows)]
fn windows_account_sid() -> Result<String, OperationError> {
    use nt_token::OwnedToken;
    use windows::Win32::Security::TOKEN_QUERY;

    OwnedToken::from_current_process(TOKEN_QUERY)
        .map_err(|_| OperationError::WindowsSecurityPolicy)?
        .user()
        .and_then(|sid| sid.to_string())
        .map_err(|_| OperationError::WindowsSecurityPolicy)
}

/// Typed operation and catalog failures.
#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    /// The operation does not exist.
    #[error("operation was not found")]
    NotFound,
    /// The legacy operation ID already exists.
    #[error("operation already exists")]
    AlreadyExists,
    /// Immutable metadata differed for an existing operation ID.
    #[error("operation submission conflicts with existing metadata")]
    SubmissionConflict,
    /// The client-declared identity used the reserved internal value.
    #[error("operation client identity is invalid")]
    InvalidClientInstanceId,
    /// Submission ownership, detached policy, or timestamps were inconsistent.
    #[error("operation submission metadata is invalid")]
    InvalidSubmission,
    /// Lease renewal was detached, terminal, or requested by another owner.
    #[error("operation lease owner does not match")]
    LeaseOwnerMismatch,
    /// Lease expiry did not advance or could not be represented.
    #[error("operation lease is invalid")]
    InvalidLease,
    /// The transition violates the lifecycle state machine.
    #[error("illegal operation transition from {from:?} to {to:?}")]
    IllegalTransition {
        /// Current state.
        from: OperationState,
        /// Requested next state.
        to: OperationState,
    },
    /// Durable cancellation prevented successful completion.
    #[error("operation cancellation won the completion race")]
    CancellationWon,
    /// Failed-state error metadata was missing or attached to another state.
    #[error("operation terminal error does not match requested state")]
    InvalidTerminalError,
    /// Progress was inconsistent or moved backward.
    #[error("operation progress is invalid")]
    InvalidProgress,
    /// Operation stage was inconsistent or moved backward.
    #[error("operation stage is invalid")]
    InvalidStage,
    /// A monotonic revision cannot be represented.
    #[error("operation revision overflowed")]
    RevisionOverflow,
    /// A concurrent writer changed the operation revision.
    #[error("operation changed concurrently")]
    ConcurrentUpdate,
    /// SQLite remained busy or locked past the bounded wait.
    #[error("operation journal is busy")]
    Busy,
    /// A bounded diagnostic exceeded its monotonic deadline.
    #[error("operation journal diagnostic timed out")]
    DiagnosticTimedOut,
    /// Persisted operation state failed validation.
    #[error("operation journal contains invalid state")]
    CorruptState,
    /// Persisted operation schema failed validation.
    #[error("operation journal schema is corrupt")]
    CorruptSchema,
    /// The SQLite file is not a Rootlight catalog.
    #[error("operation journal belongs to another application")]
    ForeignCatalog,
    /// The recorded migration digest does not match the checked migration.
    #[error("operation journal migration checksum is invalid")]
    MigrationChecksumMismatch,
    /// A prototype schema was not recognized for safe migration.
    #[error("operation journal legacy schema is unsupported")]
    UnsupportedLegacySchema,
    /// The journal was created by a newer incompatible implementation.
    #[error("operation journal schema version {observed} is unsupported")]
    UnsupportedSchemaVersion {
        /// Observed SQLite `user_version`.
        observed: u32,
    },
    /// A cancellation reason added by a future dependency is unsupported.
    #[error("operation cancellation reason is unsupported")]
    UnsupportedCancellationReason,
    /// A required mutex was poisoned by an earlier panic.
    #[error("operation journal lock was poisoned")]
    MutexPoisoned,
    /// The bundled SQLite version is below the supported security baseline.
    #[error("bundled SQLite version {observed} is unsupported")]
    UnsupportedSqlite {
        /// Observed integer SQLite version.
        observed: i32,
    },
    /// SQLite was compiled without a required feature.
    #[error("bundled SQLite compile options are unsupported")]
    UnsupportedSqliteCompileOptions,
    /// SQLite refused a required defensive connection setting.
    #[error("bundled SQLite connection configuration is unsupported")]
    UnsupportedSqliteConfiguration,
    /// Another process currently owns the catalog writer lock.
    #[error("catalog writer is already active")]
    WriterBusy,
    /// The existing lock artifact was linked, foreign-owned, or publicly accessible.
    #[error("catalog writer lock is insecure")]
    InsecureLockFile,
    /// Windows token, reparse-point, or ACL verification failed.
    #[error("catalog writer Windows security policy failed")]
    WindowsSecurityPolicy,
    /// A public error could not be serialized for durable recovery.
    #[error("operation public error serialization failed")]
    SerializePublicError(#[source] serde_json::Error),
    /// A persisted public error failed checked decoding.
    #[error("operation public error is corrupt")]
    DeserializePublicError(#[source] serde_json::Error),
    /// A serialized public error exceeded its durable bound.
    #[error("operation public error exceeds its storage limit")]
    PublicErrorTooLarge,
    /// The system clock is before the Unix epoch.
    #[error("system clock is before the supported epoch")]
    SystemClockBeforeEpoch,
    /// A wall-clock timestamp cannot be represented by SQLite.
    #[error("operation timestamp is out of range")]
    TimestampOverflow,
    /// SQLite operation failed.
    #[error("operation journal storage failed")]
    Sqlite(#[source] rusqlite::Error),
    /// Writer-lock IO failed.
    #[error("catalog writer lock IO failed")]
    LockIo(#[source] io::Error),
}

/// Shared operation journal handle used by daemon and standalone services.
pub type SharedOperationJournal = Arc<OperationJournal>;

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use super::*;
    use rootlight_error::ErrorCode;
    use tempfile::tempdir;

    fn operation(seed: u8) -> OperationId {
        OperationId::from_bytes([seed; 16])
    }

    fn attached_submission(
        operation: OperationId,
        owner: ClientInstanceId,
        deadline: Option<u64>,
        lease: u64,
    ) -> OperationSubmission {
        OperationSubmission::new(
            operation,
            OperationKind::ControlProbe,
            PlanHash::from_bytes([3; 32]),
            owner,
            false,
            deadline,
            Some(lease),
        )
        .expect("submission is valid")
    }

    #[test]
    fn operation_transitions_stage_and_progress_are_monotonic() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        journal.enqueue(operation(1)).expect("operation enqueues");
        let executing = journal
            .start_execution(operation(1))
            .expect("execution starts atomically");
        let progressed = journal
            .update_progress(
                operation(1),
                Progress::new(3, 10).expect("progress is valid"),
            )
            .expect("progress advances");
        assert!(matches!(
            journal.update_progress(
                operation(1),
                Progress::new(3, 0).expect("unknown total shape is valid"),
            ),
            Err(OperationError::InvalidProgress)
        ));
        let succeeded = journal
            .transition(operation(1), OperationState::Succeeded, None)
            .expect("operation succeeds");

        assert_eq!(executing.state, OperationState::Running);
        assert_eq!(executing.stage, OperationStage::Executing);
        assert!(executing.revision < progressed.revision);
        assert!(progressed.revision < succeeded.revision);
        assert!(
            journal
                .update_stage(operation(1), OperationStage::Cleanup)
                .is_err()
        );
        assert!(
            journal
                .update_progress(
                    operation(1),
                    Progress::new(2, 10).expect("progress shape is valid")
                )
                .is_err()
        );
    }

    #[test]
    fn lifecycle_transition_matrix_matches_the_closed_model() {
        let states = [
            OperationState::Queued,
            OperationState::Running,
            OperationState::Cancelling,
            OperationState::Succeeded,
            OperationState::Failed,
            OperationState::Cancelled,
            OperationState::Interrupted,
        ];
        let expected = [
            (OperationState::Queued, OperationState::Running),
            (OperationState::Queued, OperationState::Failed),
            (OperationState::Queued, OperationState::Interrupted),
            (OperationState::Running, OperationState::Cancelling),
            (OperationState::Running, OperationState::Succeeded),
            (OperationState::Running, OperationState::Failed),
            (OperationState::Running, OperationState::Interrupted),
            (OperationState::Cancelling, OperationState::Cancelled),
            (OperationState::Cancelling, OperationState::Failed),
            (OperationState::Cancelling, OperationState::Interrupted),
        ];

        for from in states {
            for to in states {
                assert_eq!(
                    legal_transition(from, to),
                    expected.contains(&(from, to)),
                    "unexpected lifecycle edge {from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn terminal_states_are_absorbing_and_revision_stable() {
        let terminal_states = [
            OperationState::Succeeded,
            OperationState::Failed,
            OperationState::Cancelled,
            OperationState::Interrupted,
        ];

        for (index, terminal) in terminal_states.into_iter().enumerate() {
            let seed = u8::try_from(index + 40).expect("fixture seed fits u8");
            let journal = OperationJournal::open_in_memory().expect("journal opens");
            journal
                .enqueue(operation(seed))
                .expect("operation enqueues");
            let before = match terminal {
                OperationState::Succeeded => {
                    journal
                        .start_execution(operation(seed))
                        .expect("operation starts");
                    journal
                        .transition(operation(seed), terminal, None)
                        .expect("operation succeeds")
                }
                OperationState::Failed => {
                    let error = PublicError::builder(ErrorCode::Internal, "operation failed")
                        .operation(operation(seed))
                        .build()
                        .expect("public error builds");
                    journal
                        .transition(operation(seed), terminal, Some(&error))
                        .expect("operation fails")
                }
                OperationState::Cancelled => {
                    journal
                        .request_cancellation(operation(seed), CancellationReason::ClientRequest)
                        .expect("queued cancellation wins")
                        .operation
                }
                OperationState::Interrupted => journal
                    .interrupt_deadline(operation(seed))
                    .expect("operation is interrupted"),
                OperationState::Queued | OperationState::Running | OperationState::Cancelling => {
                    unreachable!("terminal fixture set is closed")
                }
            };

            let repeated_cancel = journal
                .request_cancellation(operation(seed), CancellationReason::Shutdown)
                .expect("terminal cancellation is idempotent");
            assert!(!repeated_cancel.accepted);
            assert_eq!(repeated_cancel.operation, before);
            assert_eq!(
                journal
                    .status(operation(seed))
                    .expect("terminal status loads"),
                before
            );
            for next in terminal_states {
                let result = journal.transition(operation(seed), next, None);
                if terminal == OperationState::Interrupted && next == OperationState::Succeeded {
                    assert_eq!(result.expect("late success observes interruption"), before);
                } else if terminal == OperationState::Cancelled && next == OperationState::Succeeded
                {
                    assert!(matches!(result, Err(OperationError::CancellationWon)));
                } else {
                    assert!(
                        matches!(
                            result,
                            Err(OperationError::IllegalTransition { .. })
                                | Err(OperationError::InvalidTerminalError)
                        ),
                        "terminal edge {terminal:?} -> {next:?} unexpectedly changed state"
                    );
                }
                assert_eq!(
                    journal
                        .status(operation(seed))
                        .expect("terminal status loads"),
                    before
                );
            }
        }
    }

    #[test]
    fn concurrent_cancellation_has_one_durable_winner() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        journal.enqueue(operation(44)).expect("operation enqueues");
        journal
            .start_execution(operation(44))
            .expect("operation starts");
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for reason in [
            CancellationReason::ClientRequest,
            CancellationReason::Shutdown,
        ] {
            let journal = Arc::clone(&journal);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                journal.request_cancellation(operation(44), reason)
            }));
        }
        barrier.wait();
        let outcomes = workers
            .into_iter()
            .map(|worker| worker.join().expect("cancellation worker joins"))
            .collect::<Vec<_>>();

        let accepted = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Ok(value) if value.accepted))
            .count();
        assert_eq!(accepted, 1);
        assert!(outcomes.iter().all(|outcome| matches!(
            outcome,
            Ok(CancellationOutcome { .. }) | Err(OperationError::ConcurrentUpdate)
        )));

        let observed = journal.status(operation(44)).expect("status loads");
        assert_eq!(observed.state, OperationState::Cancelling);
        assert!(observed.cancellation_requested);
        assert_eq!(observed.revision, 3);
        assert!(matches!(
            observed.cancellation_reason,
            Some(CancellationReason::ClientRequest | CancellationReason::Shutdown)
        ));
        assert!(matches!(
            journal.transition(operation(44), OperationState::Succeeded, None),
            Err(OperationError::CancellationWon)
        ));
    }

    #[test]
    fn idempotent_submission_reuses_identical_detached_work_across_clients() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let first = OperationSubmission::new(
            operation(2),
            OperationKind::ControlProbe,
            PlanHash::from_bytes([3; 32]),
            ClientInstanceId::new([1; 16]).expect("client identity is valid"),
            true,
            None,
            None,
        )
        .expect("submission is valid");
        let submitted = journal.submit(first).expect("submission succeeds");
        assert!(submitted.inserted);
        assert_eq!(submitted.operation.revision, 1);

        let retried = OperationSubmission {
            owner: ClientInstanceId::new([2; 16]).expect("client identity is valid"),
            ..first
        };
        let outcome = journal.submit(retried).expect("detached retry succeeds");
        assert!(!outcome.inserted);
        assert_eq!(outcome.operation.revision, 1);
        assert_eq!(outcome.operation.owner, first.owner);

        let conflict = OperationSubmission {
            plan_hash: PlanHash::from_bytes([9; 32]),
            ..retried
        };
        assert!(matches!(
            journal.submit(conflict),
            Err(OperationError::SubmissionConflict)
        ));
        assert_eq!(
            journal
                .retry_status(retried)
                .expect("existing retry loads without insertion"),
            submitted.operation
        );
        assert!(matches!(
            journal.retry_status(conflict),
            Err(OperationError::SubmissionConflict)
        ));
        assert!(matches!(
            journal.retry_status(OperationSubmission::control_probe(operation(29))),
            Err(OperationError::NotFound)
        ));
    }

    #[test]
    fn explicit_deadline_retry_requires_exact_metadata() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let first = OperationSubmission::new(
            operation(30),
            OperationKind::ControlProbe,
            PlanHash::from_bytes([3; 32]),
            ClientInstanceId::new([1; 16]).expect("client identity is valid"),
            true,
            Some(100),
            None,
        )
        .expect("submission is valid");
        let submitted = journal.submit(first).expect("submission succeeds");

        let retry = OperationSubmission {
            owner: ClientInstanceId::new([2; 16]).expect("client identity is valid"),
            deadline_unix_ms: Some(200),
            ..first
        };
        assert!(matches!(
            journal.submit(retry),
            Err(OperationError::SubmissionConflict)
        ));
        assert_eq!(
            journal
                .submit(first)
                .expect("exact retry succeeds")
                .operation,
            submitted.operation
        );
    }

    #[test]
    fn reanchored_relative_retry_requires_the_original_timeout_intent() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        let journal = OperationJournal::open(&path).expect("journal opens");
        let first = OperationSubmission::new(
            operation(31),
            OperationKind::ControlProbe,
            PlanHash::from_bytes([3; 32]),
            ClientInstanceId::new([1; 16]).expect("client identity is valid"),
            true,
            Some(4_000_000_000_000),
            None,
        )
        .expect("submission is valid");
        let retry_intent = DeadlineRetry::ReanchoredRelative { timeout_ms: 100 };
        let submitted = journal
            .submit_with_deadline_retry(first, retry_intent)
            .expect("submission succeeds");
        drop(journal);
        let journal = OperationJournal::open(&path).expect("journal reopens");
        let retry = OperationSubmission {
            owner: ClientInstanceId::new([2; 16]).expect("client identity is valid"),
            deadline_unix_ms: Some(4_000_000_000_100),
            ..first
        };
        let observed = journal
            .submit_with_deadline_retry(retry, retry_intent)
            .expect("relative retry succeeds");

        assert!(!observed.inserted);
        assert_eq!(observed.operation.operation, submitted.operation.operation);
        assert_eq!(observed.operation.plan_hash, submitted.operation.plan_hash);
        assert_eq!(observed.operation.deadline_unix_ms, Some(4_000_000_000_000));
        assert!(matches!(
            journal.submit_with_deadline_retry(
                retry,
                DeadlineRetry::ReanchoredRelative { timeout_ms: 200 },
            ),
            Err(OperationError::SubmissionConflict)
        ));
        assert!(matches!(
            journal.submit(OperationSubmission {
                deadline_unix_ms: None,
                ..retry
            }),
            Err(OperationError::SubmissionConflict)
        ));
        assert!(matches!(
            journal.submit(OperationSubmission {
                deadline_unix_ms: Some(4_000_000_000_000),
                ..retry
            }),
            Err(OperationError::SubmissionConflict)
        ));
        assert!(matches!(
            journal.submit_with_deadline_retry(
                retry,
                DeadlineRetry::ReanchoredRelative { timeout_ms: 0 },
            ),
            Err(OperationError::InvalidSubmission)
        ));
    }

    #[test]
    fn attached_submission_requires_the_original_owner() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let first = attached_submission(
            operation(12),
            ClientInstanceId::new([1; 16]).expect("client identity is valid"),
            None,
            10,
        );
        journal.submit(first).expect("submission succeeds");

        let retried = OperationSubmission {
            owner: ClientInstanceId::new([2; 16]).expect("client identity is valid"),
            ..first
        };
        assert!(matches!(
            journal.submit(retried),
            Err(OperationError::SubmissionConflict)
        ));
    }

    #[test]
    fn submitted_deadline_is_audit_metadata_only() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let submission = OperationSubmission::new(
            operation(13),
            OperationKind::ControlProbe,
            PlanHash::from_bytes([3; 32]),
            ClientInstanceId::new([1; 16]).expect("client identity is valid"),
            true,
            Some(1),
            None,
        )
        .expect("submission is valid");

        let record = journal.submit(submission).expect("submission succeeds");
        let token = journal
            .cancellation_token(submission.operation)
            .expect("cancellation token exists");

        assert_eq!(record.operation.deadline_unix_ms, Some(1));
        assert_eq!(token.reason(), None);
    }

    #[test]
    fn cancellation_is_durable_idempotent_and_blocks_success() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let queued_token = journal.enqueue(operation(3)).expect("operation enqueues");
        let queued = journal
            .cancel(operation(3))
            .expect("queued cancellation succeeds");
        assert!(queued.0);
        assert_eq!(queued.1.state, OperationState::Cancelled);
        assert_eq!(
            queued.1.cancellation_reason,
            Some(CancellationReason::ClientRequest)
        );
        assert_eq!(
            queued_token.reason(),
            Some(CancellationReason::ClientRequest)
        );
        assert!(!journal.cancel(operation(3)).expect("repeat is stable").0);

        let running_token = journal.enqueue(operation(4)).expect("operation enqueues");
        journal
            .transition(operation(4), OperationState::Running, None)
            .expect("operation starts");
        let running = journal
            .cancel(operation(4))
            .expect("running cancellation succeeds");
        assert_eq!(running.1.state, OperationState::Cancelling);
        assert!(matches!(
            journal.transition(operation(4), OperationState::Succeeded, None),
            Err(OperationError::CancellationWon)
        ));
        let cancelled = journal
            .transition(operation(4), OperationState::Cancelled, None)
            .expect("cleanup completes");
        assert_eq!(cancelled.state, OperationState::Cancelled);
        assert_eq!(
            running_token.reason(),
            Some(CancellationReason::ClientRequest)
        );
        assert!(
            !journal
                .cancel(operation(4))
                .expect("terminal cancel is stable")
                .0
        );
    }

    #[test]
    fn attached_and_detached_lease_contracts_are_checked() {
        let owner = ClientInstanceId::new([5; 16]).expect("owner is valid");
        assert!(
            OperationSubmission::new(
                operation(5),
                OperationKind::ControlProbe,
                PlanHash::from_bytes([1; 32]),
                owner,
                false,
                None,
                None,
            )
            .is_err()
        );
        assert!(
            OperationSubmission::new(
                operation(5),
                OperationKind::ControlProbe,
                PlanHash::from_bytes([1; 32]),
                owner,
                true,
                None,
                Some(10),
            )
            .is_err()
        );

        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let now = unix_time_ms().expect("system clock is valid");
        let initial_expiry = now.checked_add(1_000).expect("fixture expiry fits");
        let renewed_expiry = now.checked_add(2_000).expect("fixture expiry fits");
        journal
            .submit(attached_submission(
                operation(5),
                owner,
                None,
                initial_expiry,
            ))
            .expect("attached submission succeeds");
        assert!(matches!(
            journal.renew_lease(
                operation(5),
                ClientInstanceId::new([6; 16]).expect("other owner is valid"),
                renewed_expiry
            ),
            Err(OperationError::LeaseOwnerMismatch)
        ));
        assert!(matches!(
            journal.renew_lease(operation(5), owner, now),
            Err(OperationError::InvalidLease)
        ));
        let renewed = journal
            .renew_lease(operation(5), owner, renewed_expiry)
            .expect("owner renews");
        assert_eq!(renewed.lease_expires_unix_ms, Some(renewed_expiry));
        assert_eq!(
            journal
                .renew_lease(operation(5), owner, renewed_expiry)
                .expect("exact renewal retry is idempotent"),
            renewed
        );
    }

    #[test]
    fn renewed_lease_invalidates_a_previously_selected_expiry() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let owner = ClientInstanceId::new([6; 16]).expect("owner is valid");
        let initial_expiry = unix_time_ms()
            .expect("clock is valid")
            .checked_add(30_000)
            .expect("initial expiry fits");
        journal
            .submit(attached_submission(
                operation(33),
                owner,
                None,
                initial_expiry,
            ))
            .expect("lease submits");

        let renewed_expiry = initial_expiry
            .checked_add(30_000)
            .expect("renewed expiry fits");
        journal
            .renew_lease(operation(33), owner, renewed_expiry)
            .expect("lease renews");

        let observed = journal
            .interrupt_lease(operation(33), initial_expiry)
            .expect("stale expiry is ignored");
        assert_eq!(observed.state, OperationState::Queued);
        assert_eq!(observed.lease_expires_unix_ms, Some(renewed_expiry));
    }

    #[test]
    fn cancellation_winner_rejects_lease_renewal_without_mutation() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let owner = ClientInstanceId::new([9; 16]).expect("owner is valid");
        let initial_expiry = unix_time_ms()
            .expect("clock is valid")
            .checked_add(30_000)
            .expect("initial expiry fits");
        journal
            .submit(attached_submission(
                operation(35),
                owner,
                None,
                initial_expiry,
            ))
            .expect("lease submits");
        journal
            .start_execution(operation(35))
            .expect("operation starts");
        let cancelling = journal
            .request_cancellation(operation(35), CancellationReason::ClientRequest)
            .expect("cancellation wins")
            .operation;
        let renewed_expiry = initial_expiry
            .checked_add(30_000)
            .expect("renewed expiry fits");

        assert!(matches!(
            journal.renew_lease(operation(35), owner, renewed_expiry),
            Err(OperationError::CancellationWon)
        ));
        assert_eq!(
            journal.status(operation(35)).expect("status loads"),
            cancelling
        );
    }

    #[test]
    fn restart_classifies_deadline_lease_and_generic_interruptions() {
        let connection = Connection::open_in_memory().expect("connection opens");
        let journal = OperationJournal::initialize(connection, CatalogStorage::Memory, 100)
            .expect("journal initializes");
        let owner = ClientInstanceId::new([7; 16]).expect("owner is valid");
        journal
            .submit(attached_submission(operation(6), owner, Some(90), 200))
            .expect("deadline operation submits");
        journal
            .submit(attached_submission(operation(7), owner, None, 90))
            .expect("lease operation submits");
        journal
            .submit(OperationSubmission::control_probe(operation(8)))
            .expect("generic operation submits");

        assert_eq!(
            journal
                .recover_nonterminal_batch(100, 10)
                .expect("recovery succeeds"),
            3
        );
        assert_eq!(
            journal
                .status(operation(6))
                .expect("status loads")
                .recovery_class,
            RecoveryClass::DeadlineElapsed
        );
        assert_eq!(
            journal
                .status(operation(7))
                .expect("status loads")
                .recovery_class,
            RecoveryClass::LeaseExpired
        );
        assert_eq!(
            journal
                .status(operation(8))
                .expect("status loads")
                .recovery_class,
            RecoveryClass::InterruptedByRestart
        );
    }

    #[test]
    fn restart_interrupts_cancellation_requested_work() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        let operation = operation(34);
        {
            let journal = OperationJournal::open(&path).expect("journal opens");
            journal.enqueue(operation).expect("operation enqueues");
            journal
                .start_execution(operation)
                .expect("operation starts");
            let cancelling = journal
                .request_cancellation(operation, CancellationReason::ClientRequest)
                .expect("cancellation is requested")
                .operation;
            assert_eq!(cancelling.state, OperationState::Cancelling);
        }

        let recovered = OperationJournal::open(&path).expect("journal reopens");
        let operation = recovered.status(operation).expect("status loads");
        assert_eq!(operation.state, OperationState::Interrupted);
        assert_eq!(
            operation.recovery_class,
            RecoveryClass::InterruptedByRestart
        );
        assert!(operation.cancellation_requested);
        assert_eq!(
            operation.cancellation_reason,
            Some(CancellationReason::ClientRequest)
        );
        assert_eq!(
            recovered.counts().expect("counts load"),
            OperationCounts {
                queued: 0,
                running: 0,
                cancelling: 0,
            }
        );
    }

    #[test]
    fn direct_expiry_transitions_signal_after_durable_state() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let owner = ClientInstanceId::new([8; 16]).expect("owner is valid");
        let deadline = journal
            .submit(attached_submission(operation(30), owner, Some(110), 200))
            .expect("deadline submits");
        let lease = journal
            .submit(attached_submission(operation(31), owner, None, 110))
            .expect("lease submits");
        let deadline_token = journal
            .cancellation_token(deadline.operation.operation)
            .expect("deadline token exists");
        let lease_token = journal
            .cancellation_token(lease.operation.operation)
            .expect("lease token exists");

        let deadline = journal
            .interrupt_deadline(deadline.operation.operation)
            .expect("deadline interruption succeeds");
        assert_eq!(deadline.recovery_class, RecoveryClass::DeadlineElapsed);
        assert_eq!(
            deadline_token.reason(),
            Some(CancellationReason::DeadlineExceeded)
        );

        let lease = journal
            .interrupt_lease(lease.operation.operation, 110)
            .expect("lease interruption succeeds");
        assert_eq!(lease.recovery_class, RecoveryClass::LeaseExpired);
        assert_eq!(
            lease_token.reason(),
            Some(CancellationReason::ParentCancelled)
        );
    }

    #[test]
    fn shutdown_interruption_signals_and_late_success_is_stable() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let token = journal.enqueue(operation(33)).expect("operation enqueues");
        journal
            .transition(operation(33), OperationState::Running, None)
            .expect("operation starts");

        assert_eq!(
            journal
                .interrupt_nonterminal(10)
                .expect("shutdown interruption succeeds"),
            1
        );
        assert_eq!(token.reason(), Some(CancellationReason::Shutdown));
        assert!(matches!(
            journal.cancellation_token(operation(33)),
            Err(OperationError::NotFound)
        ));
        let interrupted = journal.status(operation(33)).expect("status loads");
        assert_eq!(interrupted.state, OperationState::Interrupted);
        assert_eq!(
            interrupted.recovery_class,
            RecoveryClass::InterruptedByRestart
        );
        assert_eq!(
            journal
                .update_progress(
                    operation(33),
                    Progress::new(1, 1).expect("progress validates")
                )
                .expect("late progress observes interruption"),
            interrupted
        );
        assert_eq!(
            journal
                .transition(operation(33), OperationState::Succeeded, None)
                .expect("late success observes interruption"),
            interrupted
        );
    }

    #[cfg(windows)]
    #[test]
    fn catalog_writer_lock_uses_protected_account_dacl() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("catalog.writer.lock");
        let lock = CatalogWriterLock::acquire(&path, [8; 16]).expect("lock acquires");

        verify_private_windows_lock_dacl(&path).expect("protected account DACL verifies");

        drop(lock);
    }

    #[test]
    fn terminal_pruning_never_removes_active_records() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        for seed in 10..14 {
            journal
                .enqueue(operation(seed))
                .expect("operation enqueues");
            journal
                .transition(operation(seed), OperationState::Running, None)
                .expect("operation starts");
            journal
                .transition(operation(seed), OperationState::Succeeded, None)
                .expect("operation succeeds");
        }
        journal
            .enqueue(operation(20))
            .expect("active operation enqueues");
        journal.prune_to(2).expect("bounded pruning succeeds");

        assert!(matches!(
            journal.status(operation(10)),
            Err(OperationError::NotFound)
        ));
        assert!(matches!(
            journal.status(operation(11)),
            Err(OperationError::NotFound)
        ));
        assert!(journal.status(operation(12)).is_ok());
        assert!(journal.status(operation(13)).is_ok());
        assert_eq!(
            journal.status(operation(20)).expect("active remains").state,
            OperationState::Queued
        );
    }

    #[test]
    fn version_one_schema_rebuilds_as_strict_without_losing_rows() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        let submitted = OperationSubmission::new(
            operation(22),
            OperationKind::ControlProbe,
            PlanHash::from_bytes([7; 32]),
            ClientInstanceId::new([8; 16]).expect("client identity is valid"),
            true,
            Some(4_000_000_000_000),
            None,
        )
        .expect("submission is valid");
        {
            let connection = Connection::open(&path).expect("database opens");
            connection
                .execute_batch(VERSION_ONE_OPERATIONS_SCHEMA_SQL)
                .expect("version-one schema creates");
            connection
                .execute(
                    "INSERT INTO operations (
                        operation, kind, plan_hash, owner, detached, deadline_unix_ms,
                        lease_expires_unix_ms, state, stage, cancellation_requested,
                        cancellation_reason, recovery_class, revision, completed, total,
                        error_json, sequence
                     ) VALUES (?1, 'control_probe', ?2, ?3, 1, ?4, NULL, 'succeeded',
                               'cleanup', 0, NULL, 'not_applicable', 4, 3, 3, NULL, 1)",
                    params![
                        submitted.operation.as_bytes().as_slice(),
                        submitted.plan_hash.as_bytes().as_slice(),
                        submitted.owner.as_bytes().as_slice(),
                        u64_to_i64(submitted.deadline_unix_ms.expect("deadline exists"))
                            .expect("deadline fits SQLite"),
                    ],
                )
                .expect("version-one row inserts");
            connection
                .pragma_update(None, "user_version", 1)
                .expect("version-one marker writes");
        }

        let migrated = OperationJournal::open(&path).expect("version-one schema migrates");
        let record = migrated
            .status(submitted.operation)
            .expect("migrated row loads");
        assert_eq!(record.kind, submitted.kind);
        assert_eq!(record.plan_hash, submitted.plan_hash);
        assert_eq!(record.owner, submitted.owner);
        assert_eq!(record.deadline_unix_ms, submitted.deadline_unix_ms);
        assert_eq!(record.state, OperationState::Succeeded);
        assert_eq!(record.stage, OperationStage::Cleanup);
        assert_eq!(record.revision, 4);
        assert_eq!(
            record.progress,
            Progress::new(3, 3).expect("progress is valid")
        );
        assert!(
            table_is_strict(
                &migrated.lock_connection().expect("catalog lock is healthy"),
                "operations",
            )
            .expect("strict schema flag reads")
        );
        migrated.quick_check().expect("migrated catalog validates");
    }

    #[test]
    fn version_two_schema_migrates_without_inventing_relative_timeout_intent() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        let submitted = OperationSubmission::new(
            operation(23),
            OperationKind::ControlProbe,
            PlanHash::from_bytes([8; 32]),
            ClientInstanceId::new([9; 16]).expect("client identity is valid"),
            true,
            Some(4_000_000_000_000),
            None,
        )
        .expect("submission is valid");
        {
            let connection = Connection::open(&path).expect("database opens");
            for statement in [
                APPLICATION_META_SCHEMA_SQL,
                MIGRATIONS_SCHEMA_SQL,
                VERSION_TWO_OPERATIONS_SCHEMA_SQL,
            ] {
                connection
                    .execute_batch(statement)
                    .expect("version-two schema creates");
            }
            connection
                .execute(
                    "INSERT INTO application_meta(key, value) VALUES ('catalog_kind', ?1)",
                    [b"rootlight".as_slice()],
                )
                .expect("catalog identity inserts");
            connection
                .execute(
                    "INSERT INTO migrations(migration_id, checksum) VALUES (?1, ?2)",
                    params![
                        VERSION_TWO_SCHEMA_MIGRATION_ID,
                        VERSION_TWO_SCHEMA_MIGRATION_CHECKSUM.as_slice(),
                    ],
                )
                .expect("version-two ledger inserts");
            connection
                .execute(
                    "INSERT INTO operations (
                        operation, kind, plan_hash, owner, detached, deadline_unix_ms,
                        lease_expires_unix_ms, state, stage, cancellation_requested,
                        cancellation_reason, recovery_class, revision, completed, total,
                        error_json, sequence
                     ) VALUES (?1, 'control_probe', ?2, ?3, 1, ?4, NULL, 'succeeded',
                               'cleanup', 0, NULL, 'not_applicable', 1, 1, 1, NULL, 1)",
                    params![
                        submitted.operation.as_bytes().as_slice(),
                        submitted.plan_hash.as_bytes().as_slice(),
                        submitted.owner.as_bytes().as_slice(),
                        u64_to_i64(submitted.deadline_unix_ms.expect("deadline exists"))
                            .expect("deadline fits SQLite"),
                    ],
                )
                .expect("version-two row inserts");
            connection
                .pragma_update(None, "application_id", CATALOG_APPLICATION_ID)
                .expect("application marker writes");
            connection
                .pragma_update(None, "user_version", 2)
                .expect("version-two marker writes");
        }

        let migrated = OperationJournal::open(&path).expect("version-two schema migrates");
        assert_eq!(
            migrated
                .submit(submitted)
                .expect("exact retry remains compatible")
                .operation,
            migrated
                .status(submitted.operation)
                .expect("migrated row loads")
        );
        assert!(matches!(
            migrated.submit_with_deadline_retry(
                OperationSubmission {
                    deadline_unix_ms: Some(4_000_000_000_100),
                    ..submitted
                },
                DeadlineRetry::ReanchoredRelative { timeout_ms: 100 },
            ),
            Err(OperationError::SubmissionConflict)
        ));
        let connection = migrated.lock_connection().expect("catalog lock is healthy");
        let relative_timeout_ms = connection
            .query_row(
                "SELECT relative_timeout_ms FROM operations WHERE operation = ?1",
                [submitted.operation.as_bytes().as_slice()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .expect("retry intent reads");
        assert_eq!(relative_timeout_ms, None);
        assert_eq!(
            pragma_u32(&connection, "user_version").expect("schema version reads"),
            OPERATION_SCHEMA_VERSION
        );
    }

    #[test]
    fn prototype_schema_migrates_and_future_schema_is_rejected() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        {
            let connection = Connection::open(&path).expect("database opens");
            connection
                .execute_batch(
                    "CREATE TABLE operations (
                        operation BLOB PRIMARY KEY NOT NULL CHECK(length(operation) = 16),
                        state TEXT NOT NULL,
                        revision INTEGER NOT NULL CHECK(revision >= 1),
                        completed INTEGER NOT NULL CHECK(completed >= 0),
                        total INTEGER NOT NULL CHECK(total >= 0),
                        error_json TEXT,
                        sequence INTEGER NOT NULL UNIQUE
                     );",
                )
                .expect("prototype schema creates");
            connection
                .execute(
                    "INSERT INTO operations VALUES (?1, 'running', 1, 0, 0, NULL, 1)",
                    [operation(21).as_bytes().as_slice()],
                )
                .expect("prototype row inserts");
        }
        let migrated = OperationJournal::open(&path).expect("prototype migrates");
        let record = migrated.status(operation(21)).expect("migrated row loads");
        assert_eq!(record.kind, OperationKind::ControlProbe);
        assert_eq!(record.state, OperationState::Interrupted);
        drop(migrated);

        let connection = Connection::open(&path).expect("database reopens");
        connection
            .pragma_update(None, "user_version", OPERATION_SCHEMA_VERSION + 1)
            .expect("future version writes");
        drop(connection);
        assert!(matches!(
            OperationJournal::open(&path),
            Err(OperationError::UnsupportedSchemaVersion { .. })
        ));
    }

    #[test]
    fn sqlite_busy_and_locked_failures_map_to_stable_busy() {
        for code in [
            rusqlite::ffi::ErrorCode::DatabaseBusy,
            rusqlite::ffi::ErrorCode::DatabaseLocked,
        ] {
            let source = rusqlite::ffi::Error {
                code,
                extended_code: 0,
            };
            assert!(matches!(
                map_sqlite_error(rusqlite::Error::SqliteFailure(source, None)),
                OperationError::Busy
            ));
        }
    }

    #[test]
    fn read_only_quick_check_uses_an_independent_catalog_connection() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        let journal = OperationJournal::open(&path).expect("catalog opens");
        let connection = journal.lock_connection().expect("catalog lock is healthy");

        OperationJournal::quick_check_path(&path).expect("read-only quick check passes");
        drop(connection);
        journal
            .quick_check()
            .expect("writer connection remains healthy");
    }

    #[test]
    fn read_only_quick_check_honors_zero_timeout_before_opening() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        OperationJournal::open(&path).expect("catalog opens");

        assert!(matches!(
            OperationJournal::quick_check_path_with_timeout(&path, Duration::ZERO),
            Err(OperationError::DiagnosticTimedOut)
        ));
    }

    #[test]
    fn read_only_quick_check_rejects_an_expired_absolute_deadline() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        OperationJournal::open(&path).expect("catalog opens");

        assert!(matches!(
            OperationJournal::quick_check_path_until(
                &path,
                Instant::now()
                    .checked_sub(Duration::from_millis(1))
                    .expect("test instant subtracts"),
            ),
            Err(OperationError::DiagnosticTimedOut)
        ));
    }

    #[test]
    fn read_only_quick_check_caps_nonzero_lock_contention_to_the_caller_budget() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        OperationJournal::open(&path).expect("catalog opens");
        let lock = Connection::open(&path).expect("contending connection opens");
        lock.busy_timeout(Duration::ZERO)
            .expect("lock connection timeout configures");
        lock.execute_batch("PRAGMA journal_mode = DELETE; BEGIN EXCLUSIVE;")
            .expect("exclusive catalog lock starts");
        let budget = Duration::from_millis(25);
        let started = Instant::now();

        let result = OperationJournal::quick_check_path_with_timeout(&path, budget);
        let elapsed = started.elapsed();

        assert!(
            matches!(
                result,
                Err(OperationError::Busy | OperationError::DiagnosticTimedOut)
            ),
            "unexpected contention result: {result:?}"
        );
        assert!(
            elapsed < Duration::from_millis(150),
            "caller budget took {elapsed:?}"
        );
        drop(lock);
    }

    #[test]
    fn catalog_bootstrap_records_identity_and_defensive_policy() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        let journal = OperationJournal::open(&path).expect("catalog opens");

        journal.quick_check().expect("catalog validates");
        let connection = journal.lock_connection().expect("catalog lock is healthy");
        assert_eq!(
            pragma_u32(&connection, "application_id").expect("application ID reads"),
            CATALOG_APPLICATION_ID
        );
        assert_eq!(
            pragma_u32(&connection, "user_version").expect("schema version reads"),
            OPERATION_SCHEMA_VERSION
        );
        assert_eq!(
            catalog_storage(&connection).expect("storage mode reads"),
            CatalogStorage::Persistent
        );
        assert_eq!(
            connection
                .query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
                .expect("synchronous policy reads"),
            2
        );
        assert_eq!(
            connection
                .query_row("PRAGMA trusted_schema", [], |row| row.get::<_, i64>(0))
                .expect("trusted schema policy reads"),
            0
        );
        assert!(
            connection
                .db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)
                .expect("defensive mode reads")
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT value FROM application_meta WHERE key = 'sqlite_version'",
                    [],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .expect("SQLite version metadata reads"),
            rusqlite::version().as_bytes()
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT checksum FROM migrations WHERE migration_id = ?1",
                    [OPERATION_SCHEMA_MIGRATION_ID],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .expect("migration checksum reads"),
            OPERATION_SCHEMA_MIGRATION_CHECKSUM
        );
    }

    #[test]
    fn catalog_bootstrap_rejects_foreign_and_tampered_databases() {
        let temporary = tempdir().expect("temporary directory is available");
        let foreign_path = temporary.path().join("foreign.sqlite");
        {
            let connection = Connection::open(&foreign_path).expect("foreign database opens");
            connection
                .execute_batch("CREATE TABLE unrelated(value TEXT NOT NULL);")
                .expect("foreign schema creates");
        }
        assert!(matches!(
            OperationJournal::open(&foreign_path),
            Err(OperationError::ForeignCatalog)
        ));

        let checksum_path = temporary.path().join("checksum.sqlite");
        drop(OperationJournal::open(&checksum_path).expect("catalog creates"));
        {
            let connection = Connection::open(&checksum_path).expect("catalog reopens");
            connection
                .execute(
                    "UPDATE migrations SET checksum = zeroblob(32) WHERE migration_id = ?1",
                    [OPERATION_SCHEMA_MIGRATION_ID],
                )
                .expect("checksum is tampered");
        }
        assert!(matches!(
            OperationJournal::open(&checksum_path),
            Err(OperationError::MigrationChecksumMismatch)
        ));

        let schema_path = temporary.path().join("schema.sqlite");
        drop(OperationJournal::open(&schema_path).expect("catalog creates"));
        {
            let connection = Connection::open(&schema_path).expect("catalog reopens");
            connection
                .execute_batch(
                    "ALTER TABLE operations RENAME TO operations_original;
                     CREATE TABLE operations(value TEXT);",
                )
                .expect("schema is tampered");
        }
        assert!(matches!(
            OperationJournal::open(&schema_path),
            Err(OperationError::CorruptSchema)
        ));

        let application_path = temporary.path().join("application.sqlite");
        drop(OperationJournal::open(&application_path).expect("catalog creates"));
        {
            let connection = Connection::open(&application_path).expect("catalog reopens");
            connection
                .pragma_update(None, "application_id", CATALOG_APPLICATION_ID + 1)
                .expect("application ID is tampered");
        }
        assert!(matches!(
            OperationJournal::open(&application_path),
            Err(OperationError::ForeignCatalog)
        ));
    }

    #[test]
    fn catalog_authorizer_denies_attachments_temp_schema_and_virtual_tables() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        let journal = OperationJournal::open(&path).expect("catalog opens");
        let connection = journal.lock_connection().expect("catalog lock is healthy");

        assert!(
            connection
                .execute_batch("ATTACH ':memory:' AS other;")
                .is_err()
        );
        assert!(
            connection
                .execute_batch("CREATE TEMP TABLE forbidden(value INTEGER);")
                .is_err()
        );
        assert!(
            connection
                .execute_batch("CREATE VIRTUAL TABLE forbidden USING fts5(value);")
                .is_err()
        );
        assert!(
            connection
                .query_row(
                    "SELECT 1 FROM pragma_database_list WHERE name = 'other'",
                    [],
                    |_| Ok(()),
                )
                .optional()
                .expect("attached database list reads")
                .is_none()
        );
    }

    #[test]
    fn failed_public_error_survives_journal_restart() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        let expected = PublicError::builder(ErrorCode::Internal, "operation failed")
            .operation(operation(22))
            .build()
            .expect("public error builds");
        {
            let journal = OperationJournal::open(&path).expect("journal opens");
            journal.enqueue(operation(22)).expect("operation enqueues");
            journal
                .transition(operation(22), OperationState::Failed, Some(&expected))
                .expect("operation fails");
        }
        let reopened = OperationJournal::open(&path).expect("journal reopens");
        let actual = reopened.status(operation(22)).expect("status loads");
        assert_eq!(actual.error, Some(expected));
        assert!(actual.has_persisted_error);
    }

    #[cfg(unix)]
    #[test]
    fn catalog_writer_lock_rejects_public_or_linked_artifacts() {
        use std::{
            fs,
            os::unix::fs::{PermissionsExt as _, symlink},
        };

        let temporary = tempdir().expect("temporary directory is available");
        let public_path = temporary.path().join("public.lock");
        fs::write(&public_path, b"").expect("lock fixture writes");
        fs::set_permissions(&public_path, fs::Permissions::from_mode(0o644))
            .expect("public permissions set");
        assert!(matches!(
            CatalogWriterLock::acquire(&public_path, [1; 16]),
            Err(OperationError::InsecureLockFile)
        ));

        let target = temporary.path().join("target.lock");
        fs::write(&target, b"").expect("target writes");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))
            .expect("private permissions set");
        let linked = temporary.path().join("linked.lock");
        symlink(target, &linked).expect("link creates");
        assert!(matches!(
            CatalogWriterLock::acquire(&linked, [1; 16]),
            Err(OperationError::LockIo(_))
        ));
    }

    #[test]
    fn catalog_writer_lock_is_exclusive_and_reuses_persistent_file() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("writer.lock");
        let first = CatalogWriterLock::acquire(&path, [1; 16]).expect("first lock succeeds");
        assert!(matches!(
            CatalogWriterLock::acquire(&path, [2; 16]),
            Err(OperationError::WriterBusy)
        ));
        drop(first);
        assert!(path.exists());
        let _second = CatalogWriterLock::acquire(&path, [2; 16]).expect("second lock succeeds");
    }
}
