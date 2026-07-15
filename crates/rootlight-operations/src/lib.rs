//! Durable bounded operation lifecycle and catalog-writer arbitration.
//!
//! This crate owns monotonic operation state, progress revisions, cancellation,
//! restart classification, SQLite health checks, and the one-writer catalog lock.

#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_error::PublicError;
use rootlight_ids::OperationId;
use rusqlite::{Connection, OptionalExtension, params};

/// Maximum operation records retained after pruning.
pub const MAX_OPERATION_HISTORY: usize = 10_000;
/// Minimum bundled SQLite version required by the P1 catalog.
pub const MIN_SQLITE_VERSION_NUMBER: i32 = 3_051_003;

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
    /// Work did not reach a valid terminal result before restart or shutdown.
    Interrupted,
}

impl OperationState {
    /// Reports whether no further state transition is legal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Interrupted)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
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
    /// Current lifecycle state.
    pub state: OperationState,
    /// Monotonically increasing state or progress revision.
    pub revision: u64,
    /// Monotonic progress snapshot.
    pub progress: Progress,
    /// Stable public failure envelope retained for the current process.
    pub error: Option<PublicError>,
    /// Reports that a persisted failure envelope exists after restart.
    pub has_persisted_error: bool,
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
    /// Returns [`OperationError`] for SQLite version, compile-option, schema,
    /// integrity, or recovery failures.
    pub fn open(path: &Path) -> Result<Self, OperationError> {
        let connection = Connection::open(path).map_err(OperationError::Sqlite)?;
        Self::initialize(connection)
    }

    /// Opens an isolated in-memory journal for standalone composition and tests.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError`] for SQLite setup failures.
    pub fn open_in_memory() -> Result<Self, OperationError> {
        let connection = Connection::open_in_memory().map_err(OperationError::Sqlite)?;
        Self::initialize(connection)
    }

    fn initialize(connection: Connection) -> Result<Self, OperationError> {
        verify_sqlite(&connection)?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA journal_mode = WAL;
                 CREATE TABLE IF NOT EXISTS operations (
                     operation BLOB PRIMARY KEY NOT NULL CHECK(length(operation) = 16),
                     state TEXT NOT NULL,
                     revision INTEGER NOT NULL CHECK(revision >= 1),
                     completed INTEGER NOT NULL CHECK(completed >= 0),
                     total INTEGER NOT NULL CHECK(total >= 0),
                     error_json TEXT,
                     sequence INTEGER NOT NULL UNIQUE
                 );",
            )
            .map_err(OperationError::Sqlite)?;
        connection
            .execute(
                "UPDATE operations
                 SET state = 'interrupted', revision = revision + 1
                 WHERE state IN ('queued', 'running', 'cancelling')",
                [],
            )
            .map_err(OperationError::Sqlite)?;
        Ok(Self {
            connection: Mutex::new(connection),
            cancellations: Mutex::new(BTreeMap::new()),
            errors: Mutex::new(BTreeMap::new()),
        })
    }

    /// Inserts a new queued operation and returns its cancellation token.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::AlreadyExists`] for a reused operation ID or a
    /// typed SQLite failure.
    pub fn enqueue(&self, operation: OperationId) -> Result<Cancellation, OperationError> {
        let connection = self.lock_connection()?;
        let next_sequence: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM operations",
                [],
                |row| row.get(0),
            )
            .map_err(OperationError::Sqlite)?;
        let inserted = connection
            .execute(
                "INSERT OR IGNORE INTO operations
                 (operation, state, revision, completed, total, error_json, sequence)
                 VALUES (?1, 'queued', 1, 0, 0, NULL, ?2)",
                params![operation.as_bytes().as_slice(), next_sequence],
            )
            .map_err(OperationError::Sqlite)?;
        drop(connection);
        if inserted == 0 {
            return Err(OperationError::AlreadyExists);
        }
        let cancellation = Cancellation::new();
        self.lock_cancellations()?
            .insert(operation, cancellation.clone());
        self.prune()?;
        Ok(cancellation)
    }

    /// Applies one legal monotonic state transition.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::IllegalTransition`] for invalid state changes,
    /// [`OperationError::NotFound`] for unknown operations, or a storage error.
    pub fn transition(
        &self,
        operation: OperationId,
        next: OperationState,
        error: Option<&PublicError>,
    ) -> Result<OperationRecord, OperationError> {
        let current = self.status(operation)?;
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
        let has_error = error.is_some();
        let revision = current
            .revision
            .checked_add(1)
            .ok_or(OperationError::RevisionOverflow)?;
        let connection = self.lock_connection()?;
        let updated = connection
            .execute(
                "UPDATE operations
                 SET state = ?1, revision = ?2, error_json = ?3
                 WHERE operation = ?4 AND revision = ?5",
                params![
                    next.as_str(),
                    i64::try_from(revision).map_err(|_| OperationError::RevisionOverflow)?,
                    has_error.then_some("present"),
                    operation.as_bytes().as_slice(),
                    i64::try_from(current.revision)
                        .map_err(|_| OperationError::RevisionOverflow)?,
                ],
            )
            .map_err(OperationError::Sqlite)?;
        drop(connection);
        if updated != 1 {
            return Err(OperationError::ConcurrentUpdate);
        }
        if let Some(error) = error {
            self.lock_errors()?.insert(operation, error.clone());
        } else {
            self.lock_errors()?.remove(&operation);
        }
        if next.is_terminal() {
            self.lock_cancellations()?.remove(&operation);
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
        if current.state.is_terminal() || progress.completed < current.progress.completed {
            return Err(OperationError::InvalidProgress);
        }
        if current.progress.total != 0
            && progress.total != 0
            && current.progress.total != progress.total
        {
            return Err(OperationError::InvalidProgress);
        }
        let revision = current
            .revision
            .checked_add(1)
            .ok_or(OperationError::RevisionOverflow)?;
        let connection = self.lock_connection()?;
        let updated = connection
            .execute(
                "UPDATE operations
                 SET revision = ?1, completed = ?2, total = ?3
                 WHERE operation = ?4 AND revision = ?5",
                params![
                    i64::try_from(revision).map_err(|_| OperationError::RevisionOverflow)?,
                    progress.completed,
                    progress.total,
                    operation.as_bytes().as_slice(),
                    i64::try_from(current.revision)
                        .map_err(|_| OperationError::RevisionOverflow)?,
                ],
            )
            .map_err(OperationError::Sqlite)?;
        drop(connection);
        if updated != 1 {
            return Err(OperationError::ConcurrentUpdate);
        }
        self.status(operation)
    }

    /// Requests cooperative cancellation and persists the cancelling state.
    ///
    /// # Errors
    ///
    /// Returns a typed error for unknown or terminal operations.
    pub fn cancel(
        &self,
        operation: OperationId,
    ) -> Result<(bool, OperationRecord), OperationError> {
        let current = self.status(operation)?;
        if current.state.is_terminal() {
            return Ok((false, current));
        }
        let accepted = self
            .lock_cancellations()?
            .get(&operation)
            .is_some_and(|token| token.cancel(CancellationReason::ClientRequest));
        let updated = if current.state == OperationState::Cancelling {
            current
        } else {
            self.transition(operation, OperationState::Cancelling, None)?
        };
        Ok((accepted, updated))
    }

    /// Loads one durable operation state.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::NotFound`] for an unknown operation or a typed
    /// corruption/SQLite error.
    pub fn status(&self, operation: OperationId) -> Result<OperationRecord, OperationError> {
        let connection = self.lock_connection()?;
        let record = connection
            .query_row(
                "SELECT state, revision, completed, total, error_json
                 FROM operations WHERE operation = ?1",
                [operation.as_bytes().as_slice()],
                |row| {
                    let state: String = row.get(0)?;
                    let revision: i64 = row.get(1)?;
                    let completed: u32 = row.get(2)?;
                    let total: u32 = row.get(3)?;
                    let error_json: Option<String> = row.get(4)?;
                    Ok((state, revision, completed, total, error_json))
                },
            )
            .optional()
            .map_err(OperationError::Sqlite)?
            .ok_or(OperationError::NotFound)?;
        drop(connection);
        let revision = u64::try_from(record.1).map_err(|_| OperationError::CorruptState)?;
        let has_persisted_error = record.4.is_some();
        let error = self.lock_errors()?.get(&operation).cloned();
        Ok(OperationRecord {
            operation,
            state: OperationState::parse(&record.0)?,
            revision,
            progress: Progress::new(record.2, record.3)?,
            error,
            has_persisted_error,
        })
    }

    /// Returns the number of nonterminal operations.
    ///
    /// # Errors
    ///
    /// Returns a typed SQLite or integer-conversion failure.
    pub fn active_count(&self) -> Result<u32, OperationError> {
        let connection = self.lock_connection()?;
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM operations
                 WHERE state IN ('queued', 'running', 'cancelling')",
                [],
                |row| row.get(0),
            )
            .map_err(OperationError::Sqlite)?;
        u32::try_from(count).map_err(|_| OperationError::CorruptState)
    }

    fn prune(&self) -> Result<(), OperationError> {
        let connection = self.lock_connection()?;
        connection
            .execute(
                "DELETE FROM operations
                 WHERE operation IN (
                     SELECT operation FROM operations
                     WHERE state IN ('succeeded', 'failed', 'interrupted')
                     ORDER BY sequence DESC
                     LIMIT -1 OFFSET ?1
                 )",
                [
                    i64::try_from(MAX_OPERATION_HISTORY)
                        .map_err(|_| OperationError::CorruptState)?,
                ],
            )
            .map_err(OperationError::Sqlite)?;
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

/// Exclusive catalog-writer lock with stale-owner recovery.
#[derive(Debug)]
pub struct CatalogWriterLock {
    path: PathBuf,
    file: File,
    nonce: [u8; 16],
}

impl CatalogWriterLock {
    /// Acquires one catalog writer using create-new arbitration.
    ///
    /// Existing locks are recovered only when the recorded process is proven not
    /// alive. The nonce prevents a stale owner's cleanup from deleting a new lock.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::WriterBusy`] while another process owns the lock
    /// or a typed IO/corruption error.
    pub fn acquire(path: &Path, nonce: [u8; 16]) -> Result<Self, OperationError> {
        for _attempt in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    write_lock_record(&mut file, std::process::id(), nonce)?;
                    return Ok(Self {
                        path: path.to_path_buf(),
                        file,
                        nonce,
                    });
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                    let (pid, _) = read_lock_record(path)?;
                    if process_is_alive(pid) {
                        return Err(OperationError::WriterBusy);
                    }
                    match std::fs::remove_file(path) {
                        Ok(()) => continue,
                        Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                        Err(source) => return Err(OperationError::LockIo(source)),
                    }
                }
                Err(source) => return Err(OperationError::LockIo(source)),
            }
        }
        Err(OperationError::WriterBusy)
    }

    /// Returns the instance nonce recorded for endpoint authentication.
    #[must_use]
    pub const fn nonce(&self) -> [u8; 16] {
        self.nonce
    }
}

impl Drop for CatalogWriterLock {
    fn drop(&mut self) {
        let owned = read_lock_record(&self.path)
            .ok()
            .is_some_and(|(_, nonce)| nonce == self.nonce);
        if owned {
            let _ = std::fs::remove_file(&self.path);
        }
        let _ = self.file.flush();
    }
}

fn legal_transition(from: OperationState, to: OperationState) -> bool {
    matches!(
        (from, to),
        (OperationState::Queued, OperationState::Running)
            | (OperationState::Queued, OperationState::Cancelling)
            | (OperationState::Queued, OperationState::Failed)
            | (OperationState::Queued, OperationState::Interrupted)
            | (OperationState::Running, OperationState::Cancelling)
            | (OperationState::Running, OperationState::Succeeded)
            | (OperationState::Running, OperationState::Failed)
            | (OperationState::Running, OperationState::Interrupted)
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
    let compile_options: Vec<String> = connection
        .prepare("PRAGMA compile_options")
        .map_err(OperationError::Sqlite)?
        .query_map([], |row| row.get(0))
        .map_err(OperationError::Sqlite)?
        .collect::<Result<_, _>>()
        .map_err(OperationError::Sqlite)?;
    if compile_options
        .iter()
        .any(|option| option == "OMIT_FOREIGN_KEY")
    {
        return Err(OperationError::UnsupportedSqliteCompileOptions);
    }
    Ok(())
}

fn write_lock_record(file: &mut File, pid: u32, nonce: [u8; 16]) -> Result<(), OperationError> {
    file.set_len(0).map_err(OperationError::LockIo)?;
    file.seek(SeekFrom::Start(0))
        .map_err(OperationError::LockIo)?;
    writeln!(file, "{pid}").map_err(OperationError::LockIo)?;
    for byte in nonce {
        write!(file, "{byte:02x}").map_err(OperationError::LockIo)?;
    }
    writeln!(file).map_err(OperationError::LockIo)?;
    file.sync_all().map_err(OperationError::LockIo)
}

fn read_lock_record(path: &Path) -> Result<(u32, [u8; 16]), OperationError> {
    let mut contents = String::new();
    File::open(path)
        .map_err(OperationError::LockIo)?
        .take(128)
        .read_to_string(&mut contents)
        .map_err(OperationError::LockIo)?;
    let mut lines = contents.lines();
    let pid = lines
        .next()
        .ok_or(OperationError::CorruptLock)?
        .parse()
        .map_err(|_| OperationError::CorruptLock)?;
    let encoded = lines.next().ok_or(OperationError::CorruptLock)?;
    if encoded.len() != 32 {
        return Err(OperationError::CorruptLock);
    }
    let mut nonce = [0_u8; 16];
    for (index, byte) in nonce.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = u8::from_str_radix(&encoded[offset..offset + 2], 16)
            .map_err(|_| OperationError::CorruptLock)?;
    }
    Ok((pid, nonce))
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .creation_flags(0x0800_0000)
        .output()
        .ok()
        .is_some_and(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
}

#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

/// Typed operation and catalog failures.
#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    /// The operation does not exist.
    #[error("operation was not found")]
    NotFound,
    /// The operation ID already exists.
    #[error("operation already exists")]
    AlreadyExists,
    /// The transition violates the lifecycle state machine.
    #[error("illegal operation transition from {from:?} to {to:?}")]
    IllegalTransition {
        /// Current state.
        from: OperationState,
        /// Requested next state.
        to: OperationState,
    },
    /// Failed-state error metadata was missing or attached to another state.
    #[error("operation terminal error does not match requested state")]
    InvalidTerminalError,
    /// Progress was inconsistent or moved backward.
    #[error("operation progress is invalid")]
    InvalidProgress,
    /// A monotonic revision cannot be represented.
    #[error("operation revision overflowed")]
    RevisionOverflow,
    /// A concurrent writer changed the operation revision.
    #[error("operation changed concurrently")]
    ConcurrentUpdate,
    /// Persisted operation state failed validation.
    #[error("operation journal contains invalid state")]
    CorruptState,
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
    /// Another process currently owns the catalog writer lock.
    #[error("catalog writer is already active")]
    WriterBusy,
    /// The writer lock record was malformed.
    #[error("catalog writer lock is corrupt")]
    CorruptLock,
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
    use super::*;
    use rootlight_error::ErrorCode;
    use tempfile::tempdir;

    fn operation(seed: u8) -> OperationId {
        OperationId::from_bytes([seed; 16])
    }

    #[test]
    fn operation_transitions_and_progress_are_monotonic() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        journal.enqueue(operation(1)).expect("operation enqueues");
        let running = journal
            .transition(operation(1), OperationState::Running, None)
            .expect("operation starts");
        let progressed = journal
            .update_progress(
                operation(1),
                Progress::new(3, 10).expect("progress is valid"),
            )
            .expect("progress advances");
        let succeeded = journal
            .transition(operation(1), OperationState::Succeeded, None)
            .expect("operation succeeds");

        assert!(running.revision < progressed.revision);
        assert!(progressed.revision < succeeded.revision);
        assert!(succeeded.state.is_terminal());
        assert!(
            journal
                .transition(operation(1), OperationState::Running, None)
                .is_err()
        );
    }

    #[test]
    fn cancellation_persists_without_overwriting_terminal_state() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let token = journal.enqueue(operation(2)).expect("operation enqueues");
        journal
            .transition(operation(2), OperationState::Running, None)
            .expect("operation starts");
        let (accepted, record) = journal.cancel(operation(2)).expect("cancellation succeeds");

        assert!(accepted);
        assert_eq!(record.state, OperationState::Cancelling);
        assert_eq!(token.reason(), Some(CancellationReason::ClientRequest));
    }

    #[test]
    fn restart_classifies_nonterminal_operations_as_interrupted() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("operations.sqlite");
        {
            let journal = OperationJournal::open(&path).expect("journal opens");
            journal.enqueue(operation(3)).expect("operation enqueues");
            journal
                .transition(operation(3), OperationState::Running, None)
                .expect("operation starts");
        }
        let reopened = OperationJournal::open(&path).expect("journal reopens");
        assert_eq!(
            reopened.status(operation(3)).expect("status loads").state,
            OperationState::Interrupted
        );
    }

    #[test]
    fn failed_operations_require_a_public_error() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        journal.enqueue(operation(4)).expect("operation enqueues");
        let error = PublicError::builder(ErrorCode::Internal, "operation failed")
            .build()
            .expect("public error builds");
        let failed = journal
            .transition(operation(4), OperationState::Failed, Some(&error))
            .expect("operation fails");
        assert_eq!(failed.error, Some(error));
    }

    #[test]
    fn catalog_writer_lock_is_exclusive_and_recovers_stale_records() {
        let temporary = tempdir().expect("temporary directory is available");
        let path = temporary.path().join("writer.lock");
        let first = CatalogWriterLock::acquire(&path, [1; 16]).expect("first lock succeeds");
        assert!(matches!(
            CatalogWriterLock::acquire(&path, [2; 16]),
            Err(OperationError::WriterBusy)
        ));
        drop(first);
        let second = CatalogWriterLock::acquire(&path, [2; 16]).expect("second lock succeeds");
        assert_eq!(second.nonce(), [2; 16]);
    }
}
