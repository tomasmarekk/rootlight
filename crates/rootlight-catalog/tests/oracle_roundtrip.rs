//! Black-box persistence tests for the private catalog and sealed oracle.
//!
//! Fixtures exercise canonical rebuilds, bounded work, defensive reopen, and
//! typed rejection of invalid or tampered generation data.

use std::{fs, path::Path};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_catalog::{
    CATALOG_FILENAME, Catalog, CatalogErrorKind, ORACLE_FILENAME, OracleReader, OracleWriter,
    catalog_schema_compatibility, oracle_schema_compatibility,
};
use rootlight_ids::{FileId, GenerationIdentity, content_hash, derive_generation};
use rootlight_ir::{
    ExtensionSupport, IrDocument, IrLimits, SourceRef, SourceSpan, decode_ir_document,
};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext, GenerationMetadata,
    GenerationReader, GenerationSnapshot,
};
use rusqlite::Connection;
use tempfile::TempDir;

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
        format_version: u32::from(GENERATION_CONTRACT_VERSION.major()),
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

    let mut second_file = document.files[0].clone();
    second_file.id = FileId::from_bytes([0xf0; 20]);
    second_file.path = "src/second.rs".to_owned();
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
        .seal(generation.clone(), &context)
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
        .seal(ordered.clone(), &context)
        .expect("first rebuild seals");
    let second = OracleWriter::create_in(second_directory.path())
        .expect("second oracle target is created")
        .seal(reversed, &context)
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
              + (SELECT count(*) FROM provenance_derivations)",
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
        .seal(snapshot(fixture_documents().0), &context)
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
        .seal(snapshot(fixture_documents().0), &context)
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
    fs::write(
        directory.path().join(ORACLE_FILENAME),
        b"hostile bytes, not a sqlite database",
    )
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
    assert_eq!(catalog.schema_version(), 1);
    assert_eq!(oracle.schema_version(), 1);
    assert_ne!(catalog.application_id(), oracle.application_id());
    assert_ne!(catalog.checksum(), oracle.checksum());
    assert_eq!(GENERATION_CONTRACT_VERSION.major(), 1);
    assert_eq!(GENERATION_CONTRACT_VERSION.minor(), 0);

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
