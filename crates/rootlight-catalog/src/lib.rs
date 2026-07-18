//! Defensive SQLite ownership for Rootlight control and oracle databases.
//!
//! This slice limits control storage to schema identity; a future generation
//! manager retains staging, publication, fsync, lease, and retention ownership.

#![forbid(unsafe_code)]

mod codec;
mod read;
mod schema;
mod write;

#[cfg(test)]
extern crate self as rootlight_catalog;
#[cfg(test)]
mod oracle_roundtrip_tests;

use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
};

use rootlight_ids::ContentHash;
use rootlight_storage::{
    GenerationContext, GenerationControlError, GenerationMetadata, GenerationReader,
    GenerationResource, GenerationSnapshot, GenerationStats, GenerationValidationError,
    GenerationWriter, IdentityVerificationError, IdentityVerifiedGeneration,
};
use rusqlite::Connection;

/// Private control-plane database filename beneath the caller-owned state root.
pub const CATALOG_FILENAME: &str = "catalog.sqlite3";
/// Sealed oracle filename beneath a caller-owned generation directory.
pub const ORACLE_FILENAME: &str = "oracle.sqlite3";
/// Minimum bundled SQLite version containing the required WAL corruption fix.
pub const MIN_SQLITE_VERSION_NUMBER: i32 = 3_051_003;
/// Bounded wait in milliseconds for transient SQLite contention.
pub const SQLITE_BUSY_TIMEOUT_MS: u64 = 250;

/// SQLite schema identity pinned by generated compatibility fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaCompatibility {
    application_id: u32,
    schema_version: u32,
    checksum: ContentHash,
}

impl SchemaCompatibility {
    pub(crate) const fn new(
        application_id: u32,
        schema_version: u32,
        checksum: ContentHash,
    ) -> Self {
        Self {
            application_id,
            schema_version,
            checksum,
        }
    }

    /// Returns the SQLite application identifier.
    #[must_use]
    pub const fn application_id(self) -> u32 {
        self.application_id
    }

    /// Returns the monotonic schema version stored in `user_version`.
    #[must_use]
    pub const fn schema_version(self) -> u32 {
        self.schema_version
    }

    /// Returns the checksum of the canonical fixed DDL ledger.
    #[must_use]
    pub const fn checksum(self) -> ContentHash {
        self.checksum
    }
}

/// Returns the current private control-catalog schema identity.
#[must_use]
pub fn catalog_schema_compatibility() -> SchemaCompatibility {
    schema::control_compatibility()
}

/// Returns the current sealed-oracle schema identity.
#[must_use]
pub fn oracle_schema_compatibility() -> SchemaCompatibility {
    schema::oracle_compatibility()
}

/// Owner of one private control-plane `catalog.sqlite3`.
pub struct Catalog {
    connection: Connection,
}

impl Catalog {
    /// Opens or initializes the control catalog beneath an existing state root.
    ///
    /// This method creates only `catalog.sqlite3`; it never creates, installs,
    /// renames, syncs, or removes the caller-owned directory.
    ///
    /// # Errors
    ///
    /// Production builds return
    /// [`CatalogErrorKind::UnsupportedPrivateFileBoundary`] before filesystem
    /// mutation while ADR-026 remains proposed. Test builds additionally
    /// exercise the schema scaffold.
    pub fn open_in(state_root: &Path) -> Result<Self, CatalogError> {
        let path = state_root.join(CATALOG_FILENAME);
        let connection = schema::open_control(&path)?;
        Ok(Self { connection })
    }

    /// Revalidates the fixed control schema and SQLite integrity.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] when the database no longer matches its schema
    /// ledger, defensive settings, or integrity invariants.
    pub fn verify(&self) -> Result<(), CatalogError> {
        schema::validate_control(&self.connection)
    }
}

impl fmt::Debug for Catalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Catalog").finish_non_exhaustive()
    }
}

/// Single-use writer for one caller-owned `oracle.sqlite3`.
pub struct OracleWriter {
    connection: Connection,
    path: PathBuf,
}

impl OracleWriter {
    /// Creates a new oracle inside an existing generation directory.
    ///
    /// The target must not exist. Cleanup of a failed operation remains with
    /// the caller that owns the generation staging directory.
    ///
    /// # Errors
    ///
    /// Production builds return
    /// [`CatalogErrorKind::UnsupportedPrivateFileBoundary`] before filesystem
    /// mutation while ADR-026 remains proposed. Test builds additionally
    /// exercise existing-file, SQLite, and schema failures.
    pub fn create_in(generation_directory: &Path) -> Result<Self, CatalogError> {
        let path = generation_directory.join(ORACLE_FILENAME);
        let connection = schema::create_oracle(&path)?;
        Ok(Self { connection, path })
    }

    /// Writes a complete generation and consumes the mutable writer.
    ///
    /// The returned reader reopens the database read-only after the bounded
    /// transaction commits. This method performs no publication or filesystem
    /// durability orchestration.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] for cancellation, budget, invalid generation,
    /// corruption, contention, or storage failures.
    pub fn seal(
        self,
        generation: IdentityVerifiedGeneration,
        context: &GenerationContext<'_>,
    ) -> Result<OracleReader, CatalogError> {
        self.seal_snapshot(generation.into_snapshot(), context)
    }

    #[cfg(test)]
    pub(crate) fn seal_unverified_for_test(
        self,
        generation: GenerationSnapshot,
        context: &GenerationContext<'_>,
    ) -> Result<OracleReader, CatalogError> {
        self.seal_snapshot(generation, context)
    }

    fn seal_snapshot(
        self,
        generation: GenerationSnapshot,
        context: &GenerationContext<'_>,
    ) -> Result<OracleReader, CatalogError> {
        let Self {
            mut connection,
            path,
        } = self;
        schema::install_generation_cancellation(&connection, context)?;
        let stats = write::write_generation(&mut connection, &generation, context)?;
        drop(connection);
        let reader = OracleReader::open_path(path, context)?;
        if reader.stats != stats || reader.read(context)? != generation {
            return Err(CatalogError::new(CatalogErrorKind::Corrupt));
        }
        Ok(reader)
    }
}

impl fmt::Debug for OracleWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OracleWriter")
            .finish_non_exhaustive()
    }
}

impl GenerationWriter for OracleWriter {
    type Error = CatalogError;

    fn write_generation(
        self: Box<Self>,
        generation: IdentityVerifiedGeneration,
        context: &GenerationContext<'_>,
    ) -> Result<GenerationStats, Self::Error> {
        self.seal(generation, context).map(|reader| reader.stats)
    }
}

/// Read-only handle that reopens one sealed generation per bounded read.
pub struct OracleReader {
    path: PathBuf,
    metadata: GenerationMetadata,
    stats: GenerationStats,
}

impl OracleReader {
    /// Opens and verifies the oracle inside a generation directory.
    ///
    /// # Errors
    ///
    /// Production builds return
    /// [`CatalogErrorKind::UnsupportedPrivateFileBoundary`] before filesystem
    /// inspection while ADR-026 remains proposed. Test builds additionally
    /// exercise cancellation, compatibility, integrity, and metadata failures.
    pub fn open_in(
        generation_directory: &Path,
        context: &GenerationContext<'_>,
    ) -> Result<Self, CatalogError> {
        Self::open_path(generation_directory.join(ORACLE_FILENAME), context)
    }

    fn open_path(path: PathBuf, context: &GenerationContext<'_>) -> Result<Self, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        let connection = schema::open_oracle_reader(&path, context)?;
        let (metadata, stats) = read::read_header(&connection, context)?;
        schema::validate_oracle(&connection, context)?;
        drop(connection);
        Ok(Self {
            path,
            metadata,
            stats,
        })
    }

    /// Materializes the owned canonical generation.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] for cancellation, budget exhaustion,
    /// compatibility, corruption, or storage failures.
    pub fn read(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<GenerationSnapshot, CatalogError> {
        let connection = schema::open_oracle_reader(&self.path, context)?;
        let snapshot = read::read_generation(&connection, self.metadata, self.stats, context)?;
        schema::validate_oracle(&connection, context)?;
        Ok(snapshot)
    }

    /// Materializes only generations carrying a validated identity proof.
    ///
    /// Legacy schema version 2 oracles remain readable through [`Self::read`]
    /// for compatibility, but cannot enter the backend-neutral query contract.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogErrorKind::IdentityProofRequired`] for readable legacy
    /// generations that predate the proposed identity-claim recipe.
    pub fn read_verified(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<IdentityVerifiedGeneration, CatalogError> {
        let snapshot = self.read(context)?;
        IdentityVerifiedGeneration::verify_snapshot(snapshot, context).map_err(
            |error| match error {
                IdentityVerificationError::Control(error) => CatalogError::control(error),
                IdentityVerificationError::LegacyContract
                | IdentityVerificationError::MissingClaim => {
                    CatalogError::new(CatalogErrorKind::IdentityProofRequired)
                }
                IdentityVerificationError::InvalidGeneration
                | IdentityVerificationError::DuplicateClaim
                | IdentityVerificationError::IdentityMismatch
                | IdentityVerificationError::ManifestMismatch
                | IdentityVerificationError::UnsupportedExtension
                | IdentityVerificationError::RecipeEncoding => {
                    CatalogError::new(CatalogErrorKind::Corrupt)
                }
            },
        )
    }
}

impl fmt::Debug for OracleReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OracleReader")
            .field("metadata", &self.metadata)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl GenerationReader for OracleReader {
    type Error = CatalogError;

    fn metadata(&self) -> GenerationMetadata {
        self.metadata
    }

    fn stats(&self) -> GenerationStats {
        self.stats
    }

    fn read_generation(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<IdentityVerifiedGeneration, Self::Error> {
        self.read_verified(context)
    }
}

/// Stable, source-redacted catalog failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CatalogErrorKind {
    /// Cooperative cancellation stopped work.
    Cancelled,
    /// A declared operation resource cap was exceeded.
    BudgetExceeded {
        /// Exhausted resource family.
        resource: GenerationResource,
        /// Configured operation limit.
        limit: u64,
    },
    /// A new oracle target already existed.
    AlreadyExists,
    /// A required database did not exist.
    NotFound,
    /// The file belongs to another SQLite application.
    ForeignDatabase,
    /// The schema version is newer or otherwise incompatible.
    IncompatibleSchema,
    /// The persisted DDL checksum differs from the checked ledger.
    MigrationChecksumMismatch,
    /// SQLite or normalized IR integrity failed.
    Corrupt,
    /// SQLite remained busy past the bounded wait.
    Busy,
    /// The bundled SQLite version or compile options are unsupported.
    UnsupportedSqlite,
    /// SQLite refused a mandatory defensive setting.
    UnsupportedConfiguration,
    /// Critical extensions cannot be sealed without a persisted support policy.
    UnsupportedCriticalExtensions,
    /// The generation does not carry an accepted, validated identity proof.
    IdentityProofRequired,
    /// ADR-026 has no accepted handle-bound SQLite file implementation.
    UnsupportedPrivateFileBoundary,
    /// The database file is linked, non-regular, or not private.
    InsecureFile,
    /// Generation metadata or normalized IR was invalid.
    InvalidGeneration,
    /// An IO, SQLite, or scalar-codec operation failed.
    Storage,
}

/// Typed catalog failure with redacted `Debug` and `Display` output.
pub struct CatalogError {
    kind: CatalogErrorKind,
    source: Option<CatalogErrorSource>,
}

impl CatalogError {
    pub(crate) const fn new(kind: CatalogErrorKind) -> Self {
        Self { kind, source: None }
    }

    pub(crate) fn with_source(kind: CatalogErrorKind, source: CatalogErrorSource) -> Self {
        Self {
            kind,
            source: Some(source),
        }
    }

    pub(crate) fn control(error: GenerationControlError) -> Self {
        match error {
            GenerationControlError::Cancelled { .. } => Self::new(CatalogErrorKind::Cancelled),
            GenerationControlError::BudgetExceeded { resource, limit } => {
                Self::new(CatalogErrorKind::BudgetExceeded { resource, limit })
            }
        }
    }

    pub(crate) fn invalid_generation(error: GenerationValidationError) -> Self {
        Self::with_source(
            CatalogErrorKind::InvalidGeneration,
            CatalogErrorSource::Generation(error),
        )
    }

    pub(crate) fn corrupt_generation(error: GenerationValidationError) -> Self {
        Self::with_source(
            CatalogErrorKind::Corrupt,
            CatalogErrorSource::Generation(error),
        )
    }

    pub(crate) fn sqlite(error: rusqlite::Error) -> Self {
        let kind = match &error {
            rusqlite::Error::SqliteFailure(source, _)
                if matches!(
                    source.code,
                    rusqlite::ffi::ErrorCode::DatabaseBusy
                        | rusqlite::ffi::ErrorCode::DatabaseLocked
                ) =>
            {
                CatalogErrorKind::Busy
            }
            rusqlite::Error::SqliteFailure(source, _)
                if source.code == rusqlite::ffi::ErrorCode::OperationInterrupted =>
            {
                CatalogErrorKind::Cancelled
            }
            rusqlite::Error::SqliteFailure(source, _)
                if matches!(
                    source.code,
                    rusqlite::ffi::ErrorCode::DatabaseCorrupt
                        | rusqlite::ffi::ErrorCode::NotADatabase
                ) =>
            {
                CatalogErrorKind::Corrupt
            }
            _ => CatalogErrorKind::Storage,
        };
        Self::with_source(kind, CatalogErrorSource::Sqlite(error))
    }

    pub(crate) fn io(kind: CatalogErrorKind, error: io::Error) -> Self {
        Self::with_source(kind, CatalogErrorSource::Io(error))
    }

    pub(crate) fn json(error: serde_json::Error) -> Self {
        Self::with_source(CatalogErrorKind::Corrupt, CatalogErrorSource::Json(error))
    }

    /// Returns the stable matchable error category.
    #[must_use]
    pub const fn kind(&self) -> CatalogErrorKind {
        self.kind
    }
}

impl fmt::Debug for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.kind {
            CatalogErrorKind::Cancelled => "catalog operation was cancelled",
            CatalogErrorKind::BudgetExceeded { .. } => {
                "catalog operation exceeded its resource budget"
            }
            CatalogErrorKind::AlreadyExists => "oracle database already exists",
            CatalogErrorKind::NotFound => "oracle database was not found",
            CatalogErrorKind::ForeignDatabase => "database belongs to another application",
            CatalogErrorKind::IncompatibleSchema => "database schema is incompatible",
            CatalogErrorKind::MigrationChecksumMismatch => "database migration checksum is invalid",
            CatalogErrorKind::Corrupt => "database content is corrupt",
            CatalogErrorKind::Busy => "database is busy",
            CatalogErrorKind::UnsupportedSqlite => "bundled SQLite is unsupported",
            CatalogErrorKind::UnsupportedConfiguration => {
                "SQLite defensive configuration is unsupported"
            }
            CatalogErrorKind::UnsupportedCriticalExtensions => {
                "critical extensions are unsupported by this storage contract"
            }
            CatalogErrorKind::IdentityProofRequired => "generation identity proof is required",
            CatalogErrorKind::UnsupportedPrivateFileBoundary => {
                "private SQLite file boundary is unavailable"
            }
            CatalogErrorKind::InsecureFile => "database file is not private",
            CatalogErrorKind::InvalidGeneration => "generation data is invalid",
            CatalogErrorKind::Storage => "catalog storage operation failed",
        };
        formatter.write_str(message)
    }
}

impl Error for CatalogError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

pub(crate) enum CatalogErrorSource {
    Io(io::Error),
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Generation(GenerationValidationError),
}

impl CatalogErrorSource {
    fn class(&self) -> &'static str {
        match self {
            Self::Io(source) => {
                let _ = source.kind();
                "io"
            }
            Self::Sqlite(source) => {
                let _ = source.sqlite_error_code();
                "sqlite"
            }
            Self::Json(source) => {
                let _ = source.classify();
                "json"
            }
            Self::Generation(source) => {
                let _ = std::mem::discriminant(source);
                "generation"
            }
        }
    }
}

impl fmt::Debug for CatalogErrorSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogErrorSource")
            .field("class", &self.class())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for CatalogErrorSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("redacted catalog error source")
    }
}

impl Error for CatalogErrorSource {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_rendering_redacts_underlying_paths() {
        let secret_path = "repository-secret/source-body.rs";
        let error = CatalogError::io(
            CatalogErrorKind::Storage,
            io::Error::new(io::ErrorKind::PermissionDenied, secret_path),
        );

        assert!(!error.to_string().contains(secret_path));
        assert!(!format!("{error:?}").contains(secret_path));
        let source = error.source().expect("redacted source wrapper is retained");
        assert!(!source.to_string().contains(secret_path));
        assert!(source.source().is_none());
    }
}
