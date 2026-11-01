//! Private persistent storage for independently verified file-affine RDF.
//!
//! The expected descriptor is the caller's authority. These files do not stand
//! in for a whole oracle, a publication manifest, or a generation reader.

use std::{
    fmt,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use rootlight_ids::{ContentHash, FactId, FileId};
use rootlight_ir::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, IrDocumentUsage, IrLimits,
    RDF_STATEMENT_NAMESPACE, SourceRef, decode_rdf_statement_envelope,
};
use rootlight_storage::{
    DiskReservation, GenerationContext, GenerationControlError, GenerationMetadata,
    GenerationReadLimit, GenerationResource, IdentityVerifiedGeneration, PublicationSpaceEstimate,
    RDF_PARTITION_FORMAT_VERSION, RdfPartitionDescriptor, RdfPartitionError, ReadPage,
    UnpublishedRdfPartition, VerifiedRdfPartition,
};
use rusqlite::{Connection, Row, params};

use crate::{CatalogError, CatalogErrorKind, codec, schema, sqlite_database_path};

/// Private, non-queryable RDF awaiting authentication against the final parent.
///
/// Its file has no trusted descriptor row. The caller owns staging cleanup and
/// publication; this handle grants neither a verified descriptor nor a reader.
#[derive(Debug)]
pub struct RdfPartitionStage {
    path: PathBuf,
    metadata: GenerationMetadata,
    file: FileId,
    source_hash: ContentHash,
    source_bytes: u64,
    record_count: u64,
    payload_bytes: u64,
    content_hash: ContentHash,
    database_upper_bytes: u64,
}

impl RdfPartitionStage {
    /// Estimates database plus rollback-journal space before any file is written.
    ///
    /// The caller must admit and reserve this estimate before [`Self::write_in`].
    /// This method performs no allocation of disk space or publication.
    ///
    /// # Errors
    /// Rejects checked-size overflow or an estimate above lifecycle ceilings.
    pub fn storage_estimate(
        partition: &UnpublishedRdfPartition,
    ) -> Result<PublicationSpaceEstimate, CatalogError> {
        let database = partition_database_upper(
            u64::try_from(partition.payload_bytes()).map_err(|_| corrupt())?,
            u64::try_from(partition.records().len()).map_err(|_| corrupt())?,
        )?;
        let temporary = database
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(1024 * 1024))
            .ok_or_else(corrupt)?;
        PublicationSpaceEstimate::new(database, temporary, 0)
            .map_err(|_| CatalogError::new(CatalogErrorKind::InvalidGeneration))
    }

    /// Consumes and releases one admitted file into reserved private staging.
    ///
    /// No verified descriptor, reader, or publication authority is returned.
    /// The reservation must cover both database and temporary journal bounds.
    ///
    /// # Errors
    /// Rejects insufficient pre-write reservation, existing/insecure targets,
    /// transaction failure, checked-size overflow, or interruption.
    pub fn write_in(
        directory: &Path,
        partition: UnpublishedRdfPartition,
        reservation: DiskReservation,
        context: &GenerationContext<'_>,
    ) -> Result<Self, CatalogError> {
        partition
            .require_context(context)
            .map_err(partition_admission_error)?;
        let payload_bytes = u64::try_from(partition.payload_bytes()).map_err(|_| corrupt())?;
        let record_count = u64::try_from(partition.records().len()).map_err(|_| corrupt())?;
        let database_upper_bytes = partition_database_upper(payload_bytes, record_count)?;
        let temporary = database_upper_bytes
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(1024 * 1024))
            .ok_or_else(corrupt)?;
        if reservation.final_bytes() < database_upper_bytes
            || reservation.temporary_bytes() < temporary
        {
            return Err(CatalogError::new(CatalogErrorKind::InvalidGeneration));
        }
        let path = partition_file_path(directory, partition.file())?;
        let stage = Self {
            path,
            metadata: partition.metadata(),
            file: partition.file(),
            source_hash: partition.source_hash(),
            source_bytes: partition.source_bytes(),
            record_count,
            payload_bytes,
            content_hash: partition.content_hash(),
            database_upper_bytes,
        };
        let mut connection = schema::create_rdf_partition(&stage.path)?;
        schema::install_generation_cancellation(&connection, context)?;
        limit_partition_database(&connection, database_upper_bytes)?;
        let transaction = connection.transaction().map_err(CatalogError::sqlite)?;
        write_partition_records(&transaction, partition.records(), context)?;
        context.check().map_err(CatalogError::control)?;
        transaction.commit().map_err(CatalogError::sqlite)?;
        drop(partition);
        drop(connection);
        Ok(stage)
    }

    /// Authenticates one bounded staged file and installs its v1 trusted descriptor.
    ///
    /// At most this file is materialized. The complete parent is never fabricated
    /// or reconstructed from a fragment, and input vectors from writing are gone.
    ///
    /// # Errors
    /// Rejects changed staged content, existing descriptors, foreign parent/file
    /// ownership, unknown provenance, unchanged quotas, or typed control failures.
    pub fn authenticate(
        self,
        parent: &IdentityVerifiedGeneration,
        limits: &IrLimits,
        context: &GenerationContext<'_>,
    ) -> Result<RdfPartitionReader, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        let mut connection = schema::open_rdf_partition_staging_writer(&self.path, context)?;
        limit_partition_database(&connection, self.database_upper_bytes)?;
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(CatalogError::sqlite)?;
        schema::validate_rdf_partition_size(&transaction, self.database_upper_bytes, context)?;
        let headers: i64 = transaction
            .query_row("SELECT count(*) FROM partition_descriptor", [], |row| {
                row.get(0)
            })
            .map_err(CatalogError::sqlite)?;
        let count: i64 = transaction
            .query_row("SELECT count(*) FROM rdf_statements", [], |row| row.get(0))
            .map_err(CatalogError::sqlite)?;
        if headers != 0 || codec::nonnegative_u64(count)? != self.record_count {
            return Err(corrupt());
        }
        let mut statement = transaction.prepare("SELECT record_id, provenance_id, source, payload FROM rdf_statements ORDER BY record_id").map_err(CatalogError::sqlite)?;
        let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
        let mut records = Vec::new();
        let mut usage = IrDocumentUsage::default();
        while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
            context.check().map_err(CatalogError::control)?;
            let record = decode_record(
                row,
                self.metadata,
                self.file,
                self.source_hash,
                self.source_bytes,
                limits,
            )?;
            usage = usage
                .checked_add(
                    IrDocumentUsage::measure_extension(&record, limits).map_err(|_| corrupt())?,
                    limits,
                )
                .map_err(|_| corrupt())?;
            let count = u64::try_from(usage.extensions()).map_err(|_| corrupt())?;
            context
                .require(GenerationResource::Rows, count)
                .map_err(CatalogError::control)?;
            context
                .require(GenerationResource::SourceReferences, count)
                .map_err(CatalogError::control)?;
            let text = usage
                .total_string_bytes()
                .checked_add(usage.total_extension_bytes())
                .and_then(|bytes| u64::try_from(bytes).ok())
                .ok_or_else(corrupt)?;
            context
                .require(GenerationResource::TextBytes, text)
                .map_err(CatalogError::control)?;
            records
                .try_reserve(1)
                .map_err(|_| CatalogError::new(CatalogErrorKind::Storage))?;
            records.push(record);
        }
        drop(rows);
        drop(statement);
        let admitted = UnpublishedRdfPartition::admit(
            self.metadata,
            self.file,
            self.source_hash,
            self.source_bytes,
            records,
            limits,
            context,
        )
        .map_err(partition_admission_error)?;
        if admitted.content_hash() != self.content_hash
            || u64::try_from(admitted.payload_bytes()).map_err(|_| corrupt())? != self.payload_bytes
        {
            return Err(corrupt());
        }
        let verified = admitted
            .authenticate(parent, limits, context)
            .map_err(partition_admission_error)?;
        let descriptor = verified.descriptor().clone();
        transaction
            .execute(
                "INSERT INTO partition_descriptor(singleton, header) VALUES (1, ?1)",
                [descriptor_header(&descriptor)?],
            )
            .map_err(CatalogError::sqlite)?;
        context.check().map_err(CatalogError::control)?;
        transaction.commit().map_err(CatalogError::sqlite)?;
        drop(verified);
        drop(connection);
        RdfPartitionReader::open_path(&self.path, &descriptor, limits, context)
    }
}

fn partition_admission_error(error: RdfPartitionError) -> CatalogError {
    match error {
        RdfPartitionError::Control(error) => CatalogError::control(error),
        _ => CatalogError::new(CatalogErrorKind::InvalidGeneration),
    }
}

fn partition_database_upper(payload_bytes: u64, record_count: u64) -> Result<u64, CatalogError> {
    payload_bytes
        .checked_mul(2)
        .and_then(|bytes| {
            record_count
                .checked_mul(4096)
                .and_then(|rows| bytes.checked_add(rows))
        })
        .and_then(|bytes| bytes.checked_add(4 * 1024 * 1024))
        .ok_or_else(corrupt)
}

fn limit_partition_database(
    connection: &Connection,
    maximum_bytes: u64,
) -> Result<(), CatalogError> {
    let page_bytes: i64 = connection
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .map_err(CatalogError::sqlite)?;
    let page_bytes = codec::nonnegative_u64(page_bytes)?;
    let pages = maximum_bytes.checked_div(page_bytes).ok_or_else(corrupt)?;
    let observed: i64 = connection
        .pragma_update_and_check(None, "max_page_count", codec::sqlite_i64(pages)?, |row| {
            row.get(0)
        })
        .map_err(CatalogError::sqlite)?;
    if codec::nonnegative_u64(observed)? != pages {
        return Err(corrupt());
    }
    Ok(())
}

fn write_partition_records(
    connection: &Connection,
    records: &[ExtensionEnvelope],
    context: &GenerationContext<'_>,
) -> Result<(), CatalogError> {
    let mut insert = connection.prepare("INSERT INTO rdf_statements(record_id, provenance_id, source, payload) VALUES (?1, ?2, ?3, ?4)").map_err(CatalogError::sqlite)?;
    for record in records {
        context.check().map_err(CatalogError::control)?;
        let source = record
            .evidence
            .source
            .as_ref()
            .ok_or_else(|| CatalogError::new(CatalogErrorKind::InvalidGeneration))?;
        insert
            .execute(params![
                record.id.as_bytes().as_slice(),
                record.provenance.as_bytes().as_slice(),
                serde_json::to_vec(source).map_err(CatalogError::json)?,
                &record.payload
            ])
            .map_err(CatalogError::sqlite)?;
    }
    Ok(())
}

/// Single-use writer for a private, caller-owned RDF partition staging file.
///
/// The caller owns directory creation, failed-stage cleanup, publication, fsync,
/// and retention, exactly as for an oracle staging directory.
pub struct RdfPartitionWriter {
    connection: Connection,
    path: PathBuf,
    descriptor: RdfPartitionDescriptor,
}

impl fmt::Debug for RdfPartitionWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RdfPartitionWriter").finish_non_exhaustive()
    }
}

impl RdfPartitionWriter {
    /// Creates an exclusive, no-link, account-private partition beneath a private directory.
    ///
    /// # Errors
    /// Returns catalog storage/security errors, existing-target rejection, or interruption.
    pub fn create_in(
        directory: &Path,
        descriptor: &RdfPartitionDescriptor,
        context: &GenerationContext<'_>,
    ) -> Result<Self, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        let path = partition_path(directory, descriptor)?;
        let connection = schema::create_rdf_partition(&path)?;
        Ok(Self {
            connection,
            path,
            descriptor: descriptor.clone(),
        })
    }

    /// Persists an admitted payload, releases it, and authenticates a cold read-only snapshot.
    ///
    /// Repository source is not read; persisted typed RDF is authenticated.
    /// The caller retains the descriptor for its authenticated partition set.
    ///
    /// # Errors
    /// Rejects a mismatched capability, failed transaction, integrity drift,
    /// unchanged read quotas, or interruption. A failed stage is not published.
    pub fn seal(
        mut self,
        partition: VerifiedRdfPartition,
        limits: &IrLimits,
        context: &GenerationContext<'_>,
    ) -> Result<RdfPartitionReader, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        if partition.descriptor() != &self.descriptor {
            return Err(CatalogError::new(CatalogErrorKind::InvalidGeneration));
        }
        schema::install_generation_cancellation(&self.connection, context)?;
        let transaction = self
            .connection
            .transaction()
            .map_err(CatalogError::sqlite)?;
        transaction
            .execute(
                "INSERT INTO partition_descriptor(singleton, header) VALUES (1, ?1)",
                [descriptor_header(&self.descriptor)?],
            )
            .map_err(CatalogError::sqlite)?;
        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO rdf_statements(record_id, provenance_id, source, payload)
                 VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(CatalogError::sqlite)?;
            for record in partition.records() {
                context.check().map_err(CatalogError::control)?;
                let source = record
                    .evidence
                    .source
                    .as_ref()
                    .ok_or_else(|| CatalogError::new(CatalogErrorKind::InvalidGeneration))?;
                insert
                    .execute(params![
                        record.id.as_bytes().as_slice(),
                        record.provenance.as_bytes().as_slice(),
                        serde_json::to_vec(source).map_err(CatalogError::json)?,
                        &record.payload,
                    ])
                    .map_err(CatalogError::sqlite)?;
            }
        }
        context.check().map_err(CatalogError::control)?;
        transaction.commit().map_err(CatalogError::sqlite)?;
        drop(partition);
        drop(self.connection);
        RdfPartitionReader::open_path(&self.path, &self.descriptor, limits, context)
    }
}

/// An authenticated, bounded RDF partition pinned to one read-only SQLite snapshot.
pub struct RdfPartitionReader {
    connection: Mutex<Connection>,
    descriptor: RdfPartitionDescriptor,
    limits: IrLimits,
}

impl fmt::Debug for RdfPartitionReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RdfPartitionReader").finish_non_exhaustive()
    }
}

impl RdfPartitionReader {
    /// Cold-opens a partition against an independently retained verified descriptor.
    ///
    /// All rows are authenticated with bounded per-record ownership before any
    /// query is available. Later pages share that exact SQLite read snapshot.
    ///
    /// # Errors
    /// Rejects schema/header/content drift, insecure files, unchanged quota
    /// violations, malformed typed RDF, or interruption.
    pub fn open_in(
        directory: &Path,
        expected: &RdfPartitionDescriptor,
        limits: &IrLimits,
        context: &GenerationContext<'_>,
    ) -> Result<Self, CatalogError> {
        Self::open_path(
            &partition_path(directory, expected)?,
            expected,
            limits,
            context,
        )
    }

    fn open_path(
        path: &Path,
        expected: &RdfPartitionDescriptor,
        limits: &IrLimits,
        context: &GenerationContext<'_>,
    ) -> Result<Self, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        if expected.format_version() != RDF_PARTITION_FORMAT_VERSION
            || expected.record_count()
                > u64::try_from(limits.max_extensions).map_err(|_| corrupt())?
            || expected.payload_bytes()
                > u64::try_from(limits.max_total_extension_bytes).map_err(|_| corrupt())?
        {
            return Err(corrupt());
        }
        context
            .require(GenerationResource::Rows, expected.record_count())
            .map_err(CatalogError::control)?;
        context
            .require(
                GenerationResource::SourceReferences,
                expected.record_count(),
            )
            .map_err(CatalogError::control)?;
        let maximum_bytes = expected
            .payload_bytes()
            .checked_mul(2)
            .and_then(|bytes| {
                expected
                    .record_count()
                    .checked_mul(4096)
                    .and_then(|rows| bytes.checked_add(rows))
            })
            .and_then(|bytes| bytes.checked_add(4 * 1024 * 1024))
            .ok_or_else(corrupt)?;
        let connection = schema::open_rdf_partition_reader(path, maximum_bytes, context)?;
        let header: Vec<u8> = connection
            .query_row(
                "SELECT header FROM partition_descriptor WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(CatalogError::sqlite)?;
        if header != descriptor_header(expected)? {
            return Err(corrupt());
        }
        let count: i64 = connection
            .query_row("SELECT count(*) FROM rdf_statements", [], |row| row.get(0))
            .map_err(CatalogError::sqlite)?;
        if codec::nonnegative_u64(count)? != expected.record_count() {
            return Err(corrupt());
        }
        let mut statement = connection.prepare(
            "SELECT record_id, provenance_id, source, payload FROM rdf_statements ORDER BY record_id",
        ).map_err(CatalogError::sqlite)?;
        let mut rows = statement.query([]).map_err(CatalogError::sqlite)?;
        let mut digest = PartitionDigest {
            hasher: blake3::Hasher::new(),
            bytes: 0,
            context,
            control: None,
        };
        digest.write_bytes(b"[")?;
        let mut previous = None;
        let mut usage = IrDocumentUsage::default();
        while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
            context.check().map_err(CatalogError::control)?;
            let record = decode_record(
                row,
                expected.metadata(),
                expected.file(),
                expected.source_hash(),
                expected.source_bytes(),
                limits,
            )?;
            if previous.is_some_and(|id| id >= record.id) {
                return Err(corrupt());
            }
            if previous.is_some() {
                digest.write_bytes(b",")?;
            }
            previous = Some(record.id);
            usage = usage
                .checked_add(
                    IrDocumentUsage::measure_extension(&record, limits).map_err(|_| corrupt())?,
                    limits,
                )
                .map_err(|_| corrupt())?;
            digest.write_record(&record)?;
        }
        digest.write_bytes(b"]")?;
        let text_bytes = usage
            .total_string_bytes()
            .checked_add(usage.total_extension_bytes())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(corrupt)?;
        context
            .require(GenerationResource::TextBytes, text_bytes)
            .map_err(CatalogError::control)?;
        if ContentHash::from_bytes(*digest.hasher.finalize().as_bytes()) != expected.content_hash()
            || u64::try_from(usage.total_extension_bytes()).map_err(|_| corrupt())?
                != expected.payload_bytes()
        {
            return Err(corrupt());
        }
        drop(rows);
        drop(statement);
        context.check().map_err(CatalogError::control)?;
        Ok(Self {
            connection: Mutex::new(connection),
            descriptor: expected.clone(),
            limits: limits.clone(),
        })
    }

    /// Returns the authenticated identity and content commitment.
    #[must_use]
    pub const fn descriptor(&self) -> &RdfPartitionDescriptor {
        &self.descriptor
    }

    /// Reads stable-ID pages using an indexed cursor and one bounded probe record.
    ///
    /// Totals always describe this complete partition, independently of cursor.
    ///
    /// # Errors
    /// Returns corruption, read-budget, storage, or interruption errors.
    pub fn extensions(
        &self,
        after: Option<FactId>,
        limit: GenerationReadLimit,
        context: &GenerationContext<'_>,
    ) -> Result<ReadPage<ExtensionEnvelope>, CatalogError> {
        let ids = self.select_ids(after, usize::from(limit.get()) + 1, context)?;
        let records = self.read_selected(&ids, context)?;
        ReadPage::from_limit_plus_one(records, self.descriptor.record_count(), limit, |record| {
            record.id
        })
        .map_err(|_| corrupt())
    }

    pub(super) fn select_ids(
        &self,
        after: Option<FactId>,
        maximum: usize,
        context: &GenerationContext<'_>,
    ) -> Result<Vec<FactId>, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        require_selection_bound(maximum)?;
        let connection = self.connection.lock().map_err(|_| corrupt())?;
        schema::install_generation_cancellation(&connection, context)?;
        let columns = "SELECT record_id FROM rdf_statements";
        let sql = if after.is_some() {
            format!("{columns} WHERE record_id > ?1 ORDER BY record_id LIMIT ?2")
        } else {
            format!("{columns} ORDER BY record_id LIMIT ?1")
        };
        let mut statement = connection.prepare(&sql).map_err(CatalogError::sqlite)?;
        let probe_limit = i64::try_from(maximum).map_err(|_| corrupt())?;
        let mut rows = if let Some(cursor) = after {
            statement.query(params![cursor.as_bytes().as_slice(), probe_limit])
        } else {
            statement.query([probe_limit])
        }
        .map_err(CatalogError::sqlite)?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().map_err(CatalogError::sqlite)? {
            context.check().map_err(CatalogError::control)?;
            let id = codec::fact_id(row.get(0).map_err(CatalogError::sqlite)?)?;
            if after.is_some_and(|cursor| id <= cursor)
                || ids.last().is_some_and(|previous| *previous >= id)
                || ids.len() >= maximum
            {
                return Err(corrupt());
            }
            context
                .require(
                    GenerationResource::Rows,
                    u64::try_from(ids.len()).map_err(|_| corrupt())? + 1,
                )
                .map_err(CatalogError::control)?;
            ids.try_reserve(1)
                .map_err(|_| CatalogError::new(CatalogErrorKind::Storage))?;
            ids.push(id);
        }
        context.check().map_err(CatalogError::control)?;
        Ok(ids)
    }

    pub(super) fn read_selected(
        &self,
        ids: &[FactId],
        context: &GenerationContext<'_>,
    ) -> Result<Vec<ExtensionEnvelope>, CatalogError> {
        context.check().map_err(CatalogError::control)?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        require_selection_bound(ids.len())?;
        if ids.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(corrupt());
        }
        let connection = self.connection.lock().map_err(|_| corrupt())?;
        schema::install_generation_cancellation(&connection, context)?;
        let mut statement = connection.prepare(
            "SELECT record_id, provenance_id, source, payload FROM rdf_statements WHERE record_id = ?1 LIMIT 2"
        ).map_err(CatalogError::sqlite)?;
        let mut records = Vec::new();
        let mut usage = IrDocumentUsage::default();
        for id in ids {
            context.check().map_err(CatalogError::control)?;
            let mut rows = statement
                .query([id.as_bytes().as_slice()])
                .map_err(CatalogError::sqlite)?;
            let row = rows
                .next()
                .map_err(CatalogError::sqlite)?
                .ok_or_else(corrupt)?;
            let record = decode_record(
                row,
                self.descriptor.metadata(),
                self.descriptor.file(),
                self.descriptor.source_hash(),
                self.descriptor.source_bytes(),
                &self.limits,
            )?;
            if record.id != *id || rows.next().map_err(CatalogError::sqlite)?.is_some() {
                return Err(corrupt());
            }
            usage = usage
                .checked_add(
                    IrDocumentUsage::measure_extension(&record, &self.limits)
                        .map_err(|_| corrupt())?,
                    &self.limits,
                )
                .map_err(|_| corrupt())?;
            let text_bytes = usage
                .total_string_bytes()
                .checked_add(usage.total_extension_bytes())
                .and_then(|bytes| u64::try_from(bytes).ok())
                .ok_or_else(corrupt)?;
            context
                .require(GenerationResource::TextBytes, text_bytes)
                .map_err(CatalogError::control)?;
            context
                .require(
                    GenerationResource::SourceReferences,
                    u64::try_from(records.len()).map_err(|_| corrupt())? + 1,
                )
                .map_err(CatalogError::control)?;
            context
                .require(
                    GenerationResource::Rows,
                    u64::try_from(records.len()).map_err(|_| corrupt())? + 1,
                )
                .map_err(CatalogError::control)?;
            records
                .try_reserve(1)
                .map_err(|_| CatalogError::new(CatalogErrorKind::Storage))?;
            records.push(record);
        }
        context.check().map_err(CatalogError::control)?;
        Ok(records)
    }
}

fn require_selection_bound(maximum: usize) -> Result<(), CatalogError> {
    if maximum == 0 || maximum > usize::from(rootlight_storage::HARD_MAX_GENERATION_READ_ITEMS) + 1
    {
        return Err(corrupt());
    }
    Ok(())
}

fn partition_path(
    directory: &Path,
    descriptor: &RdfPartitionDescriptor,
) -> Result<PathBuf, CatalogError> {
    partition_file_path(directory, descriptor.file())
}

fn partition_file_path(directory: &Path, file: FileId) -> Result<PathBuf, CatalogError> {
    sqlite_database_path(directory, &format!("rdf-{file}.sqlite3"))
}

fn descriptor_header(descriptor: &RdfPartitionDescriptor) -> Result<Vec<u8>, CatalogError> {
    let metadata = descriptor.metadata();
    serde_json::to_vec(&(
        descriptor.format_version(),
        (
            metadata.contract_version().major(),
            metadata.contract_version().minor(),
            metadata.repository(),
            metadata.generation(),
            metadata.parent(),
            metadata.manifest_hash(),
            metadata.configuration_hash(),
            metadata.provider_set_hash(),
        ),
        descriptor.file(),
        descriptor.source_hash(),
        descriptor.source_bytes(),
        descriptor.record_count(),
        descriptor.payload_bytes(),
        descriptor.content_hash(),
    ))
    .map_err(CatalogError::json)
}

fn decode_record(
    row: &Row<'_>,
    metadata: GenerationMetadata,
    file: FileId,
    source_hash: ContentHash,
    source_bytes: u64,
    limits: &IrLimits,
) -> Result<ExtensionEnvelope, CatalogError> {
    let id = codec::fact_id(row.get(0).map_err(CatalogError::sqlite)?)?;
    let provenance = codec::fact_id(row.get(1).map_err(CatalogError::sqlite)?)?;
    let source_ref = row.get_ref(2).map_err(CatalogError::sqlite)?;
    let encoded_source = source_ref.as_blob().map_err(|_| corrupt())?;
    if encoded_source.len() > 4096 {
        return Err(corrupt());
    }
    let source: SourceRef = serde_json::from_slice(encoded_source).map_err(CatalogError::json)?;
    if source.repository() != metadata.repository()
        || source.generation() != metadata.generation()
        || source.span().file() != file
        || source.content_hash() != source_hash
        || source.span().end_byte() > source_bytes
        || serde_json::to_vec(&source).map_err(CatalogError::json)? != encoded_source
    {
        return Err(corrupt());
    }
    let payload_ref = row.get_ref(3).map_err(CatalogError::sqlite)?;
    let payload = payload_ref.as_str().map_err(|_| corrupt())?;
    if payload.len() > limits.max_extension_payload_bytes {
        return Err(corrupt());
    }
    let record = ExtensionEnvelope {
        id,
        repository: metadata.repository(),
        generation: metadata.generation(),
        namespace: RDF_STATEMENT_NAMESPACE.into(),
        version: "1".into(),
        criticality: ExtensionCriticality::Noncritical,
        payload: payload.to_owned(),
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: vec![FactRef::File(file)],
        },
    };
    decode_rdf_statement_envelope(&record).map_err(|_| corrupt())?;
    Ok(record)
}

struct PartitionDigest<'a, 'b> {
    hasher: blake3::Hasher,
    bytes: u64,
    context: &'a GenerationContext<'b>,
    control: Option<GenerationControlError>,
}

impl PartitionDigest<'_, '_> {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), CatalogError> {
        self.write_all(bytes).map_err(|error| {
            self.control.take().map_or_else(
                || CatalogError::io(CatalogErrorKind::Storage, error),
                CatalogError::control,
            )
        })
    }

    fn write_record(&mut self, record: &ExtensionEnvelope) -> Result<(), CatalogError> {
        serde_json::to_writer(&mut *self, record).map_err(|error| {
            self.control
                .take()
                .map_or_else(|| CatalogError::json(error), CatalogError::control)
        })
    }
}

impl Write for PartitionDigest<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let observed = self
            .bytes
            .checked_add(u64::try_from(bytes.len()).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("partition size overflow"))?;
        if let Err(error) = self
            .context
            .require(GenerationResource::EncodedTextBytes, observed)
        {
            self.control = Some(error);
            return Err(io::Error::other("partition control interrupted"));
        }
        self.hasher.update(bytes);
        self.bytes = observed;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn corrupt() -> CatalogError {
    CatalogError::new(CatalogErrorKind::Corrupt)
}
