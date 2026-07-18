//! Persistence tests for the fail-closed catalog scaffold and sealed oracle.
//!
//! Fixtures exercise canonical rebuilds, bounded work, defensive reopen, and
//! typed rejection of invalid or tampered generation data.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, CoverageReport, EncodingId,
    GenerationBoundSnapshot, IrRecord, LanguageId, MemoryAdmissionPolicy, MemoryEnforcement,
    ParseProvider, ProducerDescriptor, StreamLimits, execute_analysis,
    testkit::MockLanguageAnalyzer,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_catalog::{
    CATALOG_FILENAME, Catalog, CatalogErrorKind, ORACLE_FILENAME, OracleReader, OracleWriter,
    catalog_schema_compatibility, oracle_schema_compatibility,
};
use rootlight_ids::{
    FactId, FileId, GenerationIdentity, content_hash, derive_generation, derive_repository,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, ExtensionCriticality, ExtensionIdentifier,
    ExtensionSupport, FILE_IDENTITY_CLAIM_NAMESPACE, FactEvidence, FileIdentityClaim,
    FilePathLocator, FilePathLocatorEncoding, FileRecord, IrDocument, IrLimits, ProducerIdentity,
    ProducerKind, ProvenanceRecord, SYMBOL_IDENTITY_CLAIM_NAMESPACE, SourceRef, SourceSpan,
    decode_file_identity_claim_envelope, decode_ir_document, decode_symbol_identity_claim_envelope,
    derive_provenance_record_id, new_file_identity_claim_envelope,
    new_symbol_identity_claim_envelope,
};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext, GenerationManifestRecipe,
    GenerationMetadata, GenerationReader, GenerationSnapshot, IdentityVerificationError,
    IdentityVerifiedGeneration,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use rusqlite::Connection;
use tempfile::TempDir;

fn create_private_test_file(path: &Path) {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    drop(
        options
            .open(path)
            .expect("private test database is created"),
    );
}

fn fixture_documents() -> (
    rootlight_ir::NormalizedIrDocument,
    rootlight_ir::NormalizedIrDocument,
) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compatibility/ir/1.1/document.json");
    let encoded = fs::read(path).expect("compatibility IR fixture is readable");
    let IrDocument::NormalizedV1_1(template) =
        decode_ir_document(&encoded, &IrLimits::default(), &ExtensionSupport::default())
            .expect("compatibility IR fixture decodes")
    else {
        panic!("fixture must use normalized IR 1.1");
    };
    let generation = derive_generation(GenerationIdentity {
        repository: template.repository,
        parent: None,
        manifest_hash: content_hash(b"manifest"),
        config_hash: content_hash(b"configuration"),
        provider_set_hash: content_hash(b"providers"),
        format_version: (u32::from(GENERATION_CONTRACT_VERSION.major()) << 16)
            | u32::from(GENERATION_CONTRACT_VERSION.minor()),
    })
    .id();
    let rebound = String::from_utf8(encoded)
        .expect("compatibility IR fixture is UTF-8")
        .replace(&template.generation.to_string(), &generation.to_string());
    let IrDocument::NormalizedV1_1(mut document) = decode_ir_document(
        rebound.as_bytes(),
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("generation-rebound compatibility IR fixture decodes") else {
        panic!("fixture must use normalized IR 1.1");
    };

    document.files[0].path = "@raw-ff".to_owned();
    document.files[0].path_locator = Some(
        FilePathLocator::new(FilePathLocatorEncoding::UnixBytesV1, vec!["ff".to_owned()])
            .expect("fixture path locator is canonical"),
    );

    let mut second_file = document.files[0].clone();
    second_file.id = FileId::from_bytes([0xf0; 20]);
    second_file.path_locator = Some(
        FilePathLocator::new(
            FilePathLocatorEncoding::UnixBytesV1,
            vec!["407261772d6666".to_owned()],
        )
        .expect("second fixture path locator is canonical"),
    );
    second_file.content_hash = content_hash(b"let second = true;\n");
    second_file.byte_length = 19;
    second_file.evidence.source = Some(SourceRef::new(
        document.repository,
        document.generation,
        SourceSpan::new(second_file.id, 0, second_file.byte_length)
            .expect("fixture source span is valid"),
        second_file.content_hash,
        None,
    ));
    document.extensions[0].evidence.source = Some(SourceRef::new(
        document.repository,
        document.generation,
        SourceSpan::new(second_file.id, 1, 2).expect("opaque-only source span is valid"),
        second_file.content_hash,
        None,
    ));
    document.files.push(second_file);

    let mut reversed = document.clone();
    reversed.files.reverse();
    reversed.entities.reverse();
    reversed.occurrences.reverse();
    reversed.relations.reverse();
    reversed.provenance.reverse();
    reversed.source_mappings.reverse();
    reversed.coverage_records.reverse();
    reversed.skipped_regions.reverse();
    reversed.diagnostics.reverse();
    reversed.extensions.reverse();
    for entity in &mut reversed.entities {
        entity.flags.reverse();
        entity.evidence.derivation.reverse();
    }
    for provenance in &mut reversed.provenance {
        provenance.input_sources.reverse();
        provenance.evidence_sources.reverse();
        provenance.derivation_parents.reverse();
    }
    for mapping in &mut reversed.source_mappings {
        mapping.evidence.derivation.reverse();
    }
    for region in &mut reversed.skipped_regions {
        region.evidence.derivation.reverse();
    }
    for diagnostic in &mut reversed.diagnostics {
        diagnostic.evidence.derivation.reverse();
    }
    for extension in &mut reversed.extensions {
        extension.evidence.derivation.reverse();
    }
    (document, reversed)
}

fn metadata(document: &rootlight_ir::NormalizedIrDocument) -> GenerationMetadata {
    GenerationMetadata::new(
        document.repository,
        document.generation,
        None,
        content_hash(b"manifest"),
        content_hash(b"configuration"),
        content_hash(b"providers"),
    )
    .expect("fixture metadata is valid")
}

fn snapshot(document: rootlight_ir::NormalizedIrDocument) -> GenerationSnapshot {
    GenerationSnapshot::new(
        metadata(&document),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("fixture generation is valid")
}

fn default_context<'a>(cancellation: &'a Cancellation) -> GenerationContext<'a> {
    GenerationContext::new(cancellation, GenerationBudget::default())
}

fn write_fixture(directory: &Path) -> GenerationSnapshot {
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let generation = snapshot(fixture_documents().0);
    let reader = OracleWriter::create_in(directory)
        .expect("oracle target is created")
        .seal_unverified_for_test(generation.clone(), &context)
        .expect("fixture generation is sealed");
    assert_eq!(
        reader.read(&context).expect("sealed generation reopens"),
        generation
    );
    generation
}

#[test]
fn control_catalog_owns_only_the_exact_private_filename() {
    let directory = TempDir::new().expect("temporary state root is created");
    let catalog = Catalog::open_in(directory.path()).expect("control catalog initializes");
    catalog.verify().expect("control catalog verifies");
    drop(catalog);
    Catalog::open_in(directory.path())
        .expect("control catalog reopens")
        .verify()
        .expect("reopened control catalog verifies");

    let entries = fs::read_dir(directory.path())
        .expect("state root is readable")
        .map(|entry| {
            entry
                .expect("state entry is readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(entries, [CATALOG_FILENAME]);
}

#[test]
fn foreign_control_database_is_rejected_before_journal_mutation() {
    let directory = TempDir::new().expect("temporary state root is created");
    let path = directory.path().join(CATALOG_FILENAME);
    create_private_test_file(&path);
    let connection = Connection::open(&path).expect("foreign database is created");
    connection
        .pragma_update(None, "application_id", 7_u32)
        .expect("foreign application marker is written");
    let journal_before: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("foreign journal mode is readable");
    drop(connection);

    let error = Catalog::open_in(directory.path()).expect_err("foreign catalog is rejected");
    assert_eq!(error.kind(), CatalogErrorKind::ForeignDatabase);
    let connection = Connection::open(&path).expect("foreign database reopens");
    let journal_after: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("foreign journal mode is readable");
    assert_eq!(journal_after, journal_before);
}

#[cfg(unix)]
#[test]
fn insecure_file_precedes_database_content_classification() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = TempDir::new().expect("temporary state root is created");
    let path = directory.path().join(CATALOG_FILENAME);
    create_private_test_file(&path);
    let connection = Connection::open(&path).expect("foreign database is created");
    connection
        .pragma_update(None, "application_id", 7_u32)
        .expect("foreign application marker is written");
    drop(connection);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
        .expect("fixture permissions become insecure");

    let error = Catalog::open_in(directory.path()).expect_err("insecure catalog is rejected");
    assert_eq!(error.kind(), CatalogErrorKind::InsecureFile);
}

#[test]
fn rebuild_and_insertion_order_have_equal_logical_results() {
    let first_directory = TempDir::new().expect("first staging directory is created");
    let second_directory = TempDir::new().expect("second staging directory is created");
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let (ordered, reversed) = fixture_documents();
    let ordered = snapshot(ordered);
    let reversed = snapshot(reversed);
    assert_eq!(ordered, reversed);

    let first = OracleWriter::create_in(first_directory.path())
        .expect("first oracle target is created")
        .seal_unverified_for_test(ordered.clone(), &context)
        .expect("first rebuild seals");
    let second = OracleWriter::create_in(second_directory.path())
        .expect("second oracle target is created")
        .seal_unverified_for_test(reversed, &context)
        .expect("second rebuild seals");

    assert_eq!(first.metadata(), second.metadata());
    assert_eq!(first.stats(), second.stats());
    assert_eq!(
        first.read(&context).expect("first generation reads"),
        second.read(&context).expect("second generation reads")
    );
    assert_eq!(
        first.read(&context).expect("first generation reads"),
        ordered
    );
    let connection = Connection::open(first_directory.path().join(ORACLE_FILENAME))
        .expect("first oracle opens for cardinality verification");
    let stored_rows: i64 = connection
        .query_row(
            "SELECT
                (SELECT count(*) FROM generation_meta)
              + (SELECT count(*) FROM identity_registry)
              + (SELECT count(*) FROM source_refs)
              + (SELECT count(*) FROM provenance)
              + (SELECT count(*) FROM files)
              + (SELECT count(*) FROM entities)
              + (SELECT count(*) FROM entity_flags)
              + (SELECT count(*) FROM occurrences)
              + (SELECT count(*) FROM occurrence_candidates)
              + (SELECT count(*) FROM relations)
              + (SELECT count(*) FROM source_mappings)
              + (SELECT count(*) FROM coverage_records)
              + (SELECT count(*) FROM skipped_regions)
              + (SELECT count(*) FROM diagnostics)
              + (SELECT count(*) FROM extensions)
              + (SELECT count(*) FROM evidence_derivations)
              + (SELECT count(*) FROM provenance_sources)
              + (SELECT count(*) FROM provenance_derivations)
              + (SELECT count(*) FROM application_meta WHERE key = 'document_hash')",
            [],
            |row| row.get(0),
        )
        .expect("physical payload cardinality is readable");
    assert_eq!(
        u64::try_from(stored_rows).expect("fixture cardinality is nonnegative"),
        first.stats().stored_rows()
    );
}

#[test]
fn oracle_is_exactly_named_and_cannot_be_overwritten() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    assert!(directory.path().join(ORACLE_FILENAME).is_file());

    let error = OracleWriter::create_in(directory.path())
        .expect_err("existing oracle must not be overwritten");
    assert_eq!(error.kind(), CatalogErrorKind::AlreadyExists);
}

#[test]
fn preflight_budget_and_cancellation_leave_no_payload_rows() {
    let budget_directory = TempDir::new().expect("budget staging directory is created");
    let cancellation = Cancellation::new();
    let budget = GenerationBudget::new(1, 1, 1).expect("tiny nonzero budget is valid");
    let context = GenerationContext::new(&cancellation, budget);
    let error = OracleWriter::create_in(budget_directory.path())
        .expect("oracle schema initializes")
        .seal_unverified_for_test(snapshot(fixture_documents().0), &context)
        .expect_err("generation exceeds the tiny budget");
    assert!(matches!(
        error.kind(),
        CatalogErrorKind::BudgetExceeded { .. }
    ));
    let connection =
        Connection::open(budget_directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM generation_meta", [], |row| row.get(0))
        .expect("generation row count is readable");
    assert_eq!(rows, 0);

    let cancelled_directory = TempDir::new().expect("cancelled staging directory is created");
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    let context = default_context(&cancelled);
    let error = OracleWriter::create_in(cancelled_directory.path())
        .expect("oracle schema initializes")
        .seal_unverified_for_test(snapshot(fixture_documents().0), &context)
        .expect_err("cancelled generation is rejected");
    assert_eq!(error.kind(), CatalogErrorKind::Cancelled);
}

#[test]
fn reader_enforces_declared_budget_before_materialization() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let cancellation = Cancellation::new();
    let budget = GenerationBudget::new(1, 1, 1).expect("tiny nonzero budget is valid");
    let context = GenerationContext::new(&cancellation, budget);

    let error = OracleReader::open_in(directory.path(), &context)
        .expect_err("header cardinalities exceed the read budget");
    assert!(matches!(
        error.kind(),
        CatalogErrorKind::BudgetExceeded { .. }
    ));
}

#[test]
fn legacy_oracle_is_readable_but_cannot_enter_verified_query_contract() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    let expected = write_fixture(directory.path());
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let reader = OracleReader::open_in(directory.path(), &context)
        .expect("legacy sealed oracle opens for compatibility");

    assert_eq!(
        reader
            .read(&context)
            .expect("legacy payload remains readable"),
        expected
    );
    let error = GenerationReader::read_generation(&reader, &context)
        .expect_err("unproved legacy data cannot enter verified queries");
    assert_eq!(error.kind(), CatalogErrorKind::IdentityProofRequired);
}

#[test]
fn immutable_previous_version_sealed_oracle_remains_readable() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    let writer =
        OracleWriter::create_in(directory.path()).expect("current exact schema initializes");
    drop(writer);
    let fixture = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compatibility/storage/1.1/sealed-oracle.sql"),
    )
    .expect("immutable sealed-oracle fixture is readable");
    let connection =
        Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    connection
        .execute_batch(&fixture)
        .expect("sealed-oracle fixture materializes");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let reader = OracleReader::open_in(directory.path(), &context)
        .expect("previous generation contract opens in the current reader");
    let generation = reader
        .read(&context)
        .expect("previous sealed payload materializes");
    assert!(generation.document().files.is_empty());
    assert_eq!(reader.stats().stored_rows(), 2);
    let error = reader
        .read_verified(&context)
        .expect_err("previous contract has no accepted identity proof");
    assert_eq!(error.kind(), CatalogErrorKind::IdentityProofRequired);
}

#[test]
fn tampered_source_hash_is_detected_during_reconstruction() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let path = directory.path().join(ORACLE_FILENAME);
    let connection = Connection::open(&path).expect("oracle opens for tamper fixture");
    connection
        .execute(
            "UPDATE source_refs SET content_hash = zeroblob(32) WHERE ordinal = 0",
            [],
        )
        .expect("source hash tamper is applied");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let reader = OracleReader::open_in(directory.path(), &context)
        .expect("structurally valid tampered oracle opens");
    let error = reader
        .read(&context)
        .expect_err("semantic source tamper is rejected");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn opaque_only_source_ref_tamper_preserving_cardinality_is_rejected() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let path = directory.path().join(ORACLE_FILENAME);
    let connection = Connection::open(&path).expect("oracle opens for source-ledger tamper");
    let changed = connection
        .execute(
            "UPDATE source_refs
             SET start_byte = 2, end_byte = 3
             WHERE start_byte = 1 AND end_byte = 2",
            [],
        )
        .expect("opaque-only source identity is changed");
    assert_eq!(changed, 1);
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let reader = OracleReader::open_in(directory.path(), &context)
        .expect("structurally valid source-ledger tamper opens");
    let error = reader
        .read(&context)
        .expect_err("canonical source ledger rejects same-cardinality tamper");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn unreferenced_registry_identity_tamper_preserving_cardinality_is_rejected() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let path = directory.path().join(ORACLE_FILENAME);
    let connection = Connection::open(&path).expect("oracle opens for identity-ledger tamper");
    let changed = connection
        .execute(
            "UPDATE identity_registry
             SET identity = zeroblob(20)
             WHERE kind = 'fact'
               AND identity = (SELECT extension_id FROM extensions LIMIT 1)",
            [],
        )
        .expect("unreferenced registry identity is changed");
    assert_eq!(changed, 1);
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let reader = OracleReader::open_in(directory.path(), &context)
        .expect("structurally valid identity-ledger tamper opens");
    let error = reader
        .read(&context)
        .expect_err("canonical identity ledger rejects same-cardinality tamper");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn same_cardinality_semantic_tamper_is_detected() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let path = directory.path().join(ORACLE_FILENAME);
    let connection = Connection::open(&path).expect("oracle opens for semantic tamper fixture");
    connection
        .execute(
            "UPDATE occurrences
             SET role = CASE role WHEN 'read' THEN 'write' ELSE 'read' END",
            [],
        )
        .expect("same-cardinality semantic tamper is applied");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let reader = OracleReader::open_in(directory.path(), &context)
        .expect("structurally valid tampered oracle opens");
    let error = reader
        .read(&context)
        .expect_err("sealed document hash rejects semantic tamper");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn forged_payload_counts_are_rejected_before_record_allocation() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let path = directory.path().join(ORACLE_FILENAME);
    let connection = Connection::open(&path).expect("oracle opens for count tamper fixture");
    connection
        .execute(
            "UPDATE generation_meta SET source_ref_count = 0 WHERE singleton = 1",
            [],
        )
        .expect("source count tamper is applied");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let reader = OracleReader::open_in(directory.path(), &context)
        .expect("structurally valid count-tampered oracle opens");
    let error = reader
        .read(&context)
        .expect_err("physical source cardinality must match its header");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn critical_extensions_are_rejected_before_the_payload_transaction() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    let (mut document, _) = fixture_documents();
    document.extensions[0].criticality = ExtensionCriticality::Critical;
    let mut support = ExtensionSupport::default();
    support.supported_critical.insert(ExtensionIdentifier::new(
        document.extensions[0].namespace.clone(),
        document.extensions[0].version.clone(),
    ));
    let generation = GenerationSnapshot::new(
        metadata(&document),
        document,
        &IrLimits::default(),
        &support,
    )
    .expect("critical fixture is valid for its declared producer support");
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);

    let error = OracleWriter::create_in(directory.path())
        .expect("oracle schema initializes")
        .seal_unverified_for_test(generation, &context)
        .expect_err("storage without persisted extension policy rejects critical data");
    assert_eq!(
        error.kind(),
        CatalogErrorKind::UnsupportedCriticalExtensions
    );
    let connection =
        Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM generation_meta", [], |row| row.get(0))
        .expect("generation row count is readable");
    assert_eq!(rows, 0);
}

#[test]
fn foreign_key_damage_is_detected_on_reopen() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let connection =
        Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    connection
        .execute_batch("PRAGMA foreign_keys = OFF; DELETE FROM provenance;")
        .expect("foreign-key damage is applied");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let error = OracleReader::open_in(directory.path(), &context)
        .expect_err("damaged foreign keys are rejected");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn future_schema_version_is_rejected_without_guessing() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let connection =
        Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    connection
        .pragma_update(None, "user_version", 99_u32)
        .expect("future version marker is applied");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let error = OracleReader::open_in(directory.path(), &context)
        .expect_err("future schema must be rejected");
    assert_eq!(error.kind(), CatalogErrorKind::IncompatibleSchema);
}

#[test]
fn undeclared_schema_objects_are_rejected() {
    for sql in [
        "CREATE TABLE unexpected_table (value INTEGER) STRICT",
        "CREATE INDEX unexpected_index ON files(path)",
        "CREATE VIEW unexpected_view AS SELECT file_id FROM files",
        "CREATE TRIGGER unexpected_trigger
         AFTER INSERT ON files BEGIN SELECT 1; END",
    ] {
        let directory = TempDir::new().expect("temporary generation directory is created");
        write_fixture(directory.path());
        let connection =
            Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
        connection
            .execute_batch(sql)
            .expect("unexpected schema object is installed");
        drop(connection);

        let cancellation = Cancellation::new();
        let context = default_context(&cancellation);
        let error = OracleReader::open_in(directory.path(), &context)
            .expect_err("the exact schema ledger rejects undeclared objects");
        assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
    }
}

#[test]
fn undeclared_application_metadata_is_rejected() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let connection =
        Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    connection
        .execute(
            "INSERT INTO application_meta(key, value) VALUES ('unexpected', x'01')",
            [],
        )
        .expect("undeclared application metadata is installed");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let error = OracleReader::open_in(directory.path(), &context)
        .expect_err("the exact application metadata ledger rejects extra rows");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn undeclared_migration_is_rejected() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let connection =
        Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    let undeclared_version = oracle_schema_compatibility()
        .schema_version()
        .checked_add(1)
        .expect("fixture schema version has a successor");
    connection
        .execute(
            "INSERT INTO migrations(migration_id, checksum) VALUES (?1, zeroblob(32))",
            [undeclared_version],
        )
        .expect("undeclared migration is installed");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let error = OracleReader::open_in(directory.path(), &context)
        .expect_err("the exact migration ledger rejects extra rows");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn reserved_sqlite_prefixed_schema_objects_are_not_hidden_from_the_ledger() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let connection =
        Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    connection
        .execute_batch(
            "CREATE TABLE unexpected_reserved_object (value INTEGER) STRICT;
             PRAGMA writable_schema = ON;
             UPDATE sqlite_schema
             SET name = 'sqlite_hostile',
                 tbl_name = 'sqlite_hostile',
                 sql = 'CREATE TABLE sqlite_hostile (value INTEGER) STRICT'
             WHERE type = 'table' AND name = 'unexpected_reserved_object';
             PRAGMA writable_schema = OFF;",
        )
        .expect("reserved-prefix schema fixture is installed");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let error = OracleReader::open_in(directory.path(), &context)
        .expect_err("reserved sqlite-prefixed objects remain visible to exact validation");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn actual_text_bytes_are_checked_before_materialization() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let connection =
        Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    connection
        .execute("UPDATE files SET path = path || '-tampered'", [])
        .expect("text-byte tamper is applied");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let error = OracleReader::open_in(directory.path(), &context)
        .expect_err("actual text bytes must match the sealed header");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn excess_text_rows_are_rejected_by_bounded_preflight() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let connection =
        Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    connection
        .execute_batch(
            "WITH RECURSIVE hostile(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM hostile WHERE value < 1000
             )
             INSERT INTO files(
                 file_id, repository_id, generation_id, path, content_hash,
                 byte_length, language, encoding, generated, provenance_id,
                 evidence_source_ordinal
             )
             SELECT
                 CAST(printf('%020d', hostile.value) AS BLOB),
                 fixture.repository_id,
                 fixture.generation_id,
                 fixture.path || '-hostile-' || hostile.value,
                 fixture.content_hash,
                 fixture.byte_length,
                 fixture.language,
                 fixture.encoding,
                 fixture.generated,
                 fixture.provenance_id,
                 fixture.evidence_source_ordinal
             FROM hostile
             CROSS JOIN (SELECT * FROM files LIMIT 1) AS fixture;",
        )
        .expect("hostile extra text rows are installed");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let error = OracleReader::open_in(directory.path(), &context)
        .expect_err("bounded text preflight rejects the first excess row");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn oversized_application_metadata_is_rejected_without_returning_the_blob() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let connection =
        Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    connection
        .execute_batch(
            "PRAGMA ignore_check_constraints = ON;
             UPDATE application_meta
             SET value = zeroblob(1048576)
             WHERE key = 'database_kind';",
        )
        .expect("oversized hostile metadata is installed");
    drop(connection);

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let error = OracleReader::open_in(directory.path(), &context)
        .expect_err("oversized application metadata is rejected");
    assert_eq!(error.kind(), CatalogErrorKind::ForeignDatabase);
}

#[test]
fn arbitrary_non_database_bytes_are_typed_as_corruption() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    let path = directory.path().join(ORACLE_FILENAME);
    create_private_test_file(&path);
    fs::write(&path, b"hostile bytes, not a sqlite database")
        .expect("hostile database fixture is written");
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);

    let error = OracleReader::open_in(directory.path(), &context)
        .expect_err("non-database bytes must be rejected");
    assert_eq!(error.kind(), CatalogErrorKind::Corrupt);
}

#[test]
fn normalized_validation_rejects_hostile_source_input_before_sql() {
    let (mut document, _) = fixture_documents();
    let source = document.files[0]
        .evidence
        .source
        .as_mut()
        .expect("fixture file has source evidence");
    *source = SourceRef::new(
        source.repository(),
        source.generation(),
        source.span(),
        content_hash(b"mismatched content"),
        source.line_hint(),
    );

    let error = GenerationSnapshot::new(
        metadata(&document),
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect_err("mismatched source hash is invalid");
    assert!(error.to_string().contains("normalized IR is invalid"));
}

#[test]
fn schema_identity_is_versioned_and_source_body_columns_are_absent() {
    let catalog = catalog_schema_compatibility();
    let oracle = oracle_schema_compatibility();
    assert_eq!(catalog.schema_version(), 2);
    assert_eq!(oracle.schema_version(), 3);
    assert_ne!(catalog.application_id(), oracle.application_id());
    assert_ne!(catalog.checksum(), oracle.checksum());
    assert_eq!(GENERATION_CONTRACT_VERSION.major(), 1);
    assert_eq!(GENERATION_CONTRACT_VERSION.minor(), 2);

    let directory = TempDir::new().expect("temporary generation directory is created");
    write_fixture(directory.path());
    let connection =
        Connection::open(directory.path().join(ORACLE_FILENAME)).expect("oracle opens");
    let mut statement = connection
        .prepare(
            "SELECT name FROM pragma_table_info(?1)
             WHERE lower(name) IN ('body', 'source_body', 'source_text', 'contents')",
        )
        .expect("schema inspection query prepares");
    for table in [
        "generation_meta",
        "source_refs",
        "provenance",
        "files",
        "entities",
        "occurrences",
        "relations",
        "source_mappings",
        "coverage_records",
        "skipped_regions",
        "diagnostics",
        "extensions",
    ] {
        let names = statement
            .query_map([table], |row| row.get::<_, String>(0))
            .expect("column inspection runs")
            .collect::<Result<Vec<_>, _>>()
            .expect("column names decode");
        assert!(names.is_empty(), "{table} contains source body storage");
    }
}

#[test]
fn real_treesitter_generation_obtains_verified_capability_and_round_trips() {
    const SOURCE: &str = "pub fn answer() -> u32 { 42 }\n";
    let directory = TempDir::new().expect("temporary generation root is created");
    fs::create_dir(directory.path().join("src")).expect("source directory is created");
    fs::write(directory.path().join("src/lib.rs"), SOURCE).expect("source fixture is written");
    let generation_directory = directory.path().join("generation");
    fs::create_dir(&generation_directory).expect("generation directory is created");

    let repository = derive_repository(b"catalog-real-treesitter").id();
    let relative = RelativePath::parse(Path::new("src/lib.rs")).expect("relative path is valid");
    let root = RepositoryRoot::open(repository, directory.path()).expect("repository root opens");
    let source_snapshot = root
        .snapshot(&relative, 1024 * 1024)
        .expect("source snapshot is stable");
    let configuration_hash = content_hash(b"catalog-configuration-v2");
    let file_claim = FileIdentityClaim {
        file: source_snapshot.file(),
        repository,
        path: relative.as_str().to_owned(),
        path_identity: relative.identity_bytes().to_vec(),
        content_hash: source_snapshot.content_hash(),
        byte_length: u64::try_from(source_snapshot.content().len())
            .expect("fixture source length fits"),
    };
    let manifest_hash =
        GenerationManifestRecipe::new(repository, configuration_hash, vec![file_claim])
            .expect("manifest recipe is canonical")
            .canonical_hash()
            .expect("manifest recipe encodes");
    let provider_set_hash = content_hash(b"catalog-treesitter-provider-set-v2");
    let generation = derive_generation(GenerationIdentity {
        repository,
        parent: None,
        manifest_hash,
        config_hash: configuration_hash,
        provider_set_hash,
        format_version: (u32::from(GENERATION_CONTRACT_VERSION.major()) << 16)
            | u32::from(GENERATION_CONTRACT_VERSION.minor()),
    })
    .id();
    let source = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(
            source_snapshot.file(),
            0,
            u64::try_from(source_snapshot.content().len()).expect("fixture source length fits"),
        )
        .expect("full source span is valid"),
        source_snapshot.content_hash(),
        None,
    );

    let settings = ParserSettings::new(4096).expect("parser settings are valid");
    let runtime = RuntimeConfig::new(
        1024 * 1024,
        16_384,
        128,
        32,
        64,
        1,
        16 * 1024 * 1024,
        settings,
    )
    .expect("runtime configuration is valid");
    let provider = Arc::new(
        TreeSitterProvider::new(runtime).expect("audited Tree-sitter provider initializes"),
    );
    let parser: Arc<dyn ParseProvider> = provider;
    let analyzer = TreeSitterAnalyzer::new(
        parser,
        ProducerIdentity::new(
            "rootlight-catalog-treesitter",
            "1.0",
            content_hash(b"catalog-treesitter-configuration"),
        )
        .expect("producer identity is valid"),
        LanguageId::new("rust").expect("language identity is valid"),
        "tree-sitter-rust-0.24.2",
        content_hash(b"catalog-treesitter-binary"),
    )
    .expect("Tree-sitter analyzer is valid");
    let batch =
        BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).expect("batch limits are valid");
    let stream = StreamLimits::new(
        128,
        16_384,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .expect("stream limits are valid");
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        32,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .expect("analysis limits are valid");
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&source_snapshot, &source).expect("snapshot binds"),
        LanguageId::new("rust").expect("language identity is valid"),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"catalog-build-context")),
        &limits,
    )
    .expect("analysis request is valid")
    .with_generated_status(false);
    let analysis_cancellation = Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline is representable"),
    );
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
        &analysis_cancellation,
    )
    .expect("real Tree-sitter analysis commits");
    assert!(!output.document().entities.is_empty());

    let metadata = GenerationMetadata::new(
        repository,
        generation,
        None,
        manifest_hash,
        configuration_hash,
        provider_set_hash,
    )
    .expect("generation metadata is valid");
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let verified = IdentityVerifiedGeneration::verify(
        metadata,
        output.document().clone(),
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &context,
    )
    .expect("real Tree-sitter claims verify independently");

    let mut arbitrary_fact_document = output.document().clone();
    arbitrary_fact_document.coverage_records[0].id = FactId::from_bytes([0xee; 20]);
    let error = IdentityVerifiedGeneration::verify(
        metadata,
        arbitrary_fact_document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &context,
    )
    .expect_err("arbitrary caller fact identity is rejected");
    assert_eq!(error, IdentityVerificationError::IdentityMismatch);

    let mut mismatched_file_claim_document = output.document().clone();
    let file_claim_envelope = mismatched_file_claim_document
        .extensions
        .iter_mut()
        .find(|envelope| envelope.namespace == FILE_IDENTITY_CLAIM_NAMESPACE)
        .expect("Tree-sitter emits the file claim");
    let mut mismatched_file_claim =
        decode_file_identity_claim_envelope(file_claim_envelope).expect("file claim decodes");
    mismatched_file_claim.path = "src/not-lib.rs".to_owned();
    *file_claim_envelope = new_file_identity_claim_envelope(
        &mismatched_file_claim,
        generation,
        file_claim_envelope.provenance,
        file_claim_envelope
            .evidence
            .source
            .clone()
            .expect("file claim has direct source"),
    )
    .expect("mutated claim has a self-consistent envelope ID");
    let error = IdentityVerifiedGeneration::verify(
        metadata,
        mismatched_file_claim_document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &context,
    )
    .expect_err("file claim that disagrees with the typed record is rejected");
    assert_eq!(error, IdentityVerificationError::IdentityMismatch);

    let mut mismatched_symbol_claim_document = output.document().clone();
    let symbol_claim_envelope = mismatched_symbol_claim_document
        .extensions
        .iter_mut()
        .find(|envelope| envelope.namespace == SYMBOL_IDENTITY_CLAIM_NAMESPACE)
        .expect("Tree-sitter emits symbol claims");
    let mut mismatched_symbol_claim =
        decode_symbol_identity_claim_envelope(symbol_claim_envelope).expect("symbol claim decodes");
    mismatched_symbol_claim.container = None;
    *symbol_claim_envelope = new_symbol_identity_claim_envelope(
        &mismatched_symbol_claim,
        generation,
        symbol_claim_envelope.provenance,
        symbol_claim_envelope
            .evidence
            .source
            .clone()
            .expect("symbol claim has direct source"),
    )
    .expect("mutated claim has a self-consistent envelope ID");
    let error = IdentityVerifiedGeneration::verify(
        metadata,
        mismatched_symbol_claim_document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &context,
    )
    .expect_err("symbol claim that disagrees with the typed record is rejected");
    assert_eq!(error, IdentityVerificationError::IdentityMismatch);

    let reader = OracleWriter::create_in(&generation_directory)
        .expect("oracle target is created")
        .seal(verified, &context)
        .expect("verified Tree-sitter generation seals");
    let queried = reader
        .read_generation(&context)
        .expect("backend-neutral verified query succeeds");
    assert_eq!(queried.document(), output.document());
}

#[test]
fn independent_sdk_producer_uses_the_same_identity_verifier() {
    const SOURCE: &str = "opaque mock input\n";
    let directory = TempDir::new().expect("temporary source root is created");
    fs::write(directory.path().join("input.txt"), SOURCE).expect("source fixture is written");
    let repository = derive_repository(b"catalog-independent-sdk").id();
    let relative = RelativePath::parse(Path::new("input.txt")).expect("relative path is valid");
    let root = RepositoryRoot::open(repository, directory.path()).expect("repository root opens");
    let source_snapshot = root
        .snapshot(&relative, 1024 * 1024)
        .expect("source snapshot is stable");
    let configuration_hash = content_hash(b"independent-sdk-configuration");
    let file_claim = FileIdentityClaim {
        file: source_snapshot.file(),
        repository,
        path: relative.as_str().to_owned(),
        path_identity: relative.identity_bytes().to_vec(),
        content_hash: source_snapshot.content_hash(),
        byte_length: u64::try_from(source_snapshot.content().len())
            .expect("fixture source length fits"),
    };
    let manifest_hash =
        GenerationManifestRecipe::new(repository, configuration_hash, vec![file_claim.clone()])
            .expect("manifest recipe is canonical")
            .canonical_hash()
            .expect("manifest recipe encodes");
    let provider_set_hash = content_hash(b"independent-sdk-provider-set");
    let generation = derive_generation(GenerationIdentity {
        repository,
        parent: None,
        manifest_hash,
        config_hash: configuration_hash,
        provider_set_hash,
        format_version: (u32::from(GENERATION_CONTRACT_VERSION.major()) << 16)
            | u32::from(GENERATION_CONTRACT_VERSION.minor()),
    })
    .id();
    let source = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(
            source_snapshot.file(),
            0,
            u64::try_from(source_snapshot.content().len()).expect("fixture source length fits"),
        )
        .expect("full source span is valid"),
        source_snapshot.content_hash(),
        None,
    );
    let producer = ProducerIdentity::new(
        "rootlight-independent-sdk",
        "1.0",
        content_hash(b"independent-sdk-producer-configuration"),
    )
    .expect("producer identity is valid");
    let build_context = BuildContextIdentity::new(content_hash(b"independent-sdk-build"));
    let mut provenance = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        producer_kind: ProducerKind::Rule,
        producer: producer.clone(),
        binary_digest: content_hash(b"independent-sdk-binary"),
        frontend_version: Some("mock-sdk-1.0".to_owned()),
        language: "text".to_owned(),
        tier: AnalysisTier::TierC,
        build_context,
        input_sources: vec![source.clone()],
        evidence_sources: vec![source.clone()],
        derivation_parents: Vec::new(),
        rule: Some("fixture".to_owned()),
    };
    provenance.id =
        derive_provenance_record_id(&provenance).expect("typed provenance recipe encodes");
    let file = FileRecord {
        id: source_snapshot.file(),
        repository,
        generation,
        path: relative.as_str().to_owned(),
        path_locator: Some(relative.to_locator()),
        content_hash: source_snapshot.content_hash(),
        byte_length: u64::try_from(source_snapshot.content().len())
            .expect("fixture source length fits"),
        language: "text".to_owned(),
        encoding: "utf-8".to_owned(),
        generated: false,
        provenance: provenance.id,
        evidence: FactEvidence {
            source: Some(source.clone()),
            derivation: Vec::new(),
        },
    };
    let claim_envelope =
        new_file_identity_claim_envelope(&file_claim, generation, provenance.id, source.clone())
            .expect("file identity claim envelope is canonical");
    let language = LanguageId::new("text").expect("language identity is valid");
    let descriptor = ProducerDescriptor::new(
        producer,
        ProducerKind::Rule,
        language.clone(),
        AnalysisTier::TierC,
        MemoryEnforcement::AccountedInProcess,
        true,
    );
    let analyzer = MockLanguageAnalyzer::new(
        descriptor,
        vec![
            IrRecord::File(file),
            IrRecord::Provenance(provenance),
            IrRecord::Extension(claim_envelope),
        ],
        CoverageReport::new(
            AnalysisTier::TierC,
            CoverageStatus::Complete,
            source_snapshot.content().len(),
            source_snapshot.content().len(),
            0,
            Vec::new(),
        )
        .expect("mock coverage is valid"),
        0,
    );
    let batch =
        BatchThresholds::new(16, 1024 * 1024, 16, 128 * 1024).expect("batch limits are valid");
    let stream = StreamLimits::new(16, 128, 4 * 1024 * 1024, 16, 128 * 1024, 1024 * 1024, batch)
        .expect("stream limits are valid");
    let limits = AnalysisLimits::new(
        1024 * 1024,
        1024,
        64,
        8,
        4 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .expect("analysis limits are valid");
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&source_snapshot, &source).expect("snapshot binds"),
        language,
        AnalysisTier::TierC,
        build_context,
        &limits,
    )
    .expect("analysis request is valid")
    .with_generated_status(false);
    let analysis_cancellation = Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline is representable"),
    );
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::RequireHardOrAccounted,
        &analysis_cancellation,
    )
    .expect("independent SDK analyzer commits");
    let metadata = GenerationMetadata::new(
        repository,
        generation,
        None,
        manifest_hash,
        configuration_hash,
        provider_set_hash,
    )
    .expect("generation metadata is valid");
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let verified = IdentityVerifiedGeneration::verify(
        metadata,
        output.document().clone(),
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &context,
    )
    .expect("the shared storage verifier accepts the SDK producer");
    assert_eq!(verified.document(), output.document());
}
