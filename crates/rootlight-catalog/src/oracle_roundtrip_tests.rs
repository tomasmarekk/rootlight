//! Persistence tests for the fail-closed catalog scaffold and sealed oracle.
//!
//! Fixtures exercise canonical rebuilds, bounded work, defensive reopen, and
//! typed rejection of invalid or tampered generation data.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
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
    CATALOG_FILENAME, Catalog, CatalogError, CatalogErrorKind, EphemeralOracleWriter,
    ORACLE_FILENAME, OracleReader, OracleWriter, catalog_schema_compatibility,
    oracle_schema_compatibility,
};
use rootlight_ids::{
    FactId, FileId, GenerationIdentity, content_hash, derive_generation, derive_repository,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityFlag, ExtensionCriticality,
    ExtensionIdentifier, ExtensionSupport, FILE_IDENTITY_CLAIM_NAMESPACE, FactDomain, FactEvidence,
    FileIdentityClaim, FilePathLocator, FilePathLocatorEncoding, FileRecord, IrDocument, IrLimits,
    OccurrenceRole, ProducerIdentity, ProducerKind, ProvenanceRecord, RelationPredicate,
    SYMBOL_IDENTITY_CLAIM_NAMESPACE, SourceRef, SourceSpan, decode_file_identity_claim_envelope,
    decode_ir_document, decode_symbol_identity_claim_envelope, derive_coverage_record_id,
    derive_occurrence_record_id, derive_provenance_record_id, derive_relation_record_id,
    new_file_identity_claim_envelope, new_symbol_identity_claim_envelope,
};
use rootlight_storage::{
    CoverageReadRequest, GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext,
    GenerationManifestRecipe, GenerationMetadata, GenerationReadLimit, GenerationReader,
    GenerationResource, GenerationSnapshot, IdentityVerificationError, IdentityVerifiedGeneration,
    OccurrenceReadRequest, ReadPageCompleteness, RelationReadDirection, RelationReadRequest,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use rusqlite::{
    Connection,
    hooks::{AuthAction, Authorization},
};
use tempfile::TempDir as RawTempDir;

struct TempDir(RawTempDir);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            // The default macOS temp path starts at the `/var` alias, which the
            // no-follow SQLite boundary deliberately rejects.
            tempfile::Builder::new()
                .prefix("rootlight-catalog-")
                .tempdir_in("/private/tmp")
                .map(Self)
        }
        #[cfg(not(target_os = "macos"))]
        {
            RawTempDir::new().map(Self)
        }
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

fn create_private_test_file(path: &Path) {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows::Win32::Storage::FileSystem::{
            FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, WRITE_DAC,
        };

        options
            .access_mode((FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC).0)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .expect("private test database is created");
    crate::schema::protect_private_test_file(&mut file).expect("test database is protected");
}

fn canonical_test_root(directory: &TempDir) -> PathBuf {
    // macOS exposes its default temporary directory through the `/var` alias,
    // which the VFS no-follow boundary intentionally rejects.
    fs::canonicalize(directory.path()).expect("temporary root canonicalizes")
}

pub(super) fn fixture_documents() -> (
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
    document.entities[0].flags = vec![EntityFlag::Test, EntityFlag::Synthetic];
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

pub(super) fn snapshot(document: rootlight_ir::NormalizedIrDocument) -> GenerationSnapshot {
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

fn rdf_partition_parent_document(
    file_count: usize,
    text: &str,
    statement_count: usize,
    configuration: &[u8],
) -> (GenerationMetadata, rootlight_ir::NormalizedIrDocument) {
    let mut source_digest = blake3::Hasher::new();
    for _ in 0..statement_count {
        source_digest.update(text.as_bytes());
        source_digest.update(b"\n");
    }
    let source_hash = rootlight_ids::ContentHash::from_bytes(*source_digest.finalize().as_bytes());
    let byte_length = u64::try_from((text.len() + 1) * statement_count).unwrap();
    rdf_partition_parent_document_for_source(file_count, source_hash, byte_length, configuration)
}

fn rdf_partition_parent_document_for_source(
    file_count: usize,
    source_hash: rootlight_ids::ContentHash,
    byte_length: u64,
    configuration: &[u8],
) -> (GenerationMetadata, rootlight_ir::NormalizedIrDocument) {
    let repository = derive_repository(b"catalog-rdf-partition").id();
    let configuration_hash = content_hash(configuration);
    let provider_set_hash = content_hash(b"catalog-rdf-partition-producer");
    let claims: Vec<_> = (0..file_count)
        .map(|index| {
            let path = RelativePath::parse(Path::new(&format!("data/{index}.nt"))).unwrap();
            let file = rootlight_ids::derive_file(rootlight_ids::FileIdentity {
                repository,
                path_identity: path.identity_bytes(),
            })
            .id();
            FileIdentityClaim {
                file,
                repository,
                path: path.as_str().into(),
                path_identity: path.identity_bytes().to_vec(),
                content_hash: source_hash,
                byte_length,
            }
        })
        .collect();
    let manifest_hash =
        GenerationManifestRecipe::new(repository, configuration_hash, claims.clone())
            .unwrap()
            .canonical_hash()
            .unwrap();
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
    let metadata = GenerationMetadata::new(
        repository,
        generation,
        None,
        manifest_hash,
        configuration_hash,
        provider_set_hash,
    )
    .unwrap();
    let sources: Vec<_> = claims
        .iter()
        .map(|claim| {
            SourceRef::new(
                repository,
                generation,
                SourceSpan::new(claim.file, 0, byte_length).unwrap(),
                source_hash,
                None,
            )
        })
        .collect();
    let mut provenance = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        producer_kind: ProducerKind::Parser,
        producer: ProducerIdentity::new("catalog-rdf-partition", "1", configuration_hash).unwrap(),
        binary_digest: content_hash(b"catalog-rdf-partition-test"),
        frontend_version: Some("ntriples-1".into()),
        language: "ntriples".into(),
        tier: AnalysisTier::TierC,
        build_context: BuildContextIdentity::new(configuration_hash),
        input_sources: sources.clone(),
        evidence_sources: sources.clone(),
        derivation_parents: Vec::new(),
        rule: None,
    };
    provenance.input_sources.sort();
    provenance.evidence_sources.sort();
    provenance.id = derive_provenance_record_id(&provenance).unwrap();
    let mut document = rootlight_ir::NormalizedIrDocument::empty(repository, generation);
    for (claim, source) in claims.iter().zip(sources) {
        let path = RelativePath::parse(Path::new(&claim.path)).unwrap();
        document.files.push(FileRecord {
            id: claim.file,
            repository,
            generation,
            path: claim.path.clone(),
            path_locator: Some(path.to_locator()),
            content_hash: source_hash,
            byte_length,
            language: "ntriples".into(),
            encoding: "utf-8".into(),
            generated: false,
            provenance: provenance.id,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: Vec::new(),
            },
        });
        document.extensions.push(
            new_file_identity_claim_envelope(claim, generation, provenance.id, source).unwrap(),
        );
    }
    document.provenance.push(provenance);
    (metadata, document)
}

fn rdf_partition_parent(
    file_count: usize,
    text: &str,
    statement_count: usize,
    configuration: &[u8],
) -> IdentityVerifiedGeneration {
    let (metadata, document) =
        rdf_partition_parent_document(file_count, text, statement_count, configuration);
    let cancellation = Cancellation::new();
    IdentityVerifiedGeneration::verify(
        metadata,
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &default_context(&cancellation),
    )
    .unwrap()
}

fn rdf_partition_records(
    parent: &IdentityVerifiedGeneration,
    file: FileId,
    text: &str,
    count: usize,
) -> Vec<rootlight_ir::ExtensionEnvelope> {
    let file = parent.snapshot().find_file(file).unwrap();
    rdf_partition_records_claimed(parent.metadata(), file, text, count)
}

fn rdf_partition_records_claimed(
    metadata: GenerationMetadata,
    file: &FileRecord,
    text: &str,
    count: usize,
) -> Vec<rootlight_ir::ExtensionEnvelope> {
    let predicate_start = text.find('<').unwrap();
    let predicate_end = text.find('>').unwrap() + 1;
    let object_start = predicate_end + 1;
    let object_end = text.len() - 2;
    (0..count)
        .map(|index| {
            let start = u64::try_from(index * (text.len() + 1)).unwrap();
            let span = |first, last| {
                SourceSpan::new(
                    file.id,
                    start + u64::try_from(first).unwrap(),
                    start + u64::try_from(last).unwrap(),
                )
                .unwrap()
            };
            let source = SourceRef::new(
                metadata.repository(),
                metadata.generation(),
                span(0, text.len()),
                file.content_hash,
                Some(
                    rootlight_ir::LineRange::new(
                        u64::try_from(index + 1).unwrap(),
                        u64::try_from(index + 1).unwrap(),
                    )
                    .unwrap(),
                ),
            );
            let statement = rootlight_ir::RdfStatement::from_ntriples(
                source,
                text,
                [
                    span(0, predicate_start - 1),
                    span(predicate_start, predicate_end),
                    span(object_start, object_end),
                ],
            )
            .unwrap();
            rootlight_ir::new_rdf_statement_envelope(file.provenance, &statement).unwrap()
        })
        .collect()
}

#[test]
fn rdf_partition_cold_pages_preserve_typed_records_and_source_hints() {
    use crate::{RdfPartitionReader, RdfPartitionWriter};
    use rootlight_storage::VerifiedRdfPartition;
    let text = "_:a <urn:p> \"caf\\u00E9\"@en .";
    let parent = rdf_partition_parent(2, text, 5, b"cold-rdf");
    let file = parent.document().files[0].id;
    let records = rdf_partition_records(&parent, file, text, 5);
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let limits = IrLimits::default();
    let partition =
        VerifiedRdfPartition::verify(&parent, file, records.clone(), &limits, &context).unwrap();
    let descriptor = partition.descriptor().clone();
    let mut expected = records;
    expected.sort_by_key(|record| record.id);
    let directory = TempDir::new().unwrap();
    let reader = RdfPartitionWriter::create_in(directory.path(), &descriptor, &context)
        .unwrap()
        .seal(partition, &limits, &context)
        .unwrap();
    drop(reader);
    drop(parent);
    let reader =
        RdfPartitionReader::open_in(directory.path(), &descriptor, &limits, &context).unwrap();
    let mut observed = Vec::new();
    let mut cursor = None;
    loop {
        let page = reader
            .extensions(cursor, GenerationReadLimit::new(2).unwrap(), &context)
            .unwrap();
        assert_eq!(page.total_available(), 5);
        let next = page.next_cursor();
        observed.extend(page.into_items());
        if next.is_none() {
            break;
        }
        cursor = next;
    }
    assert_eq!(observed, expected);
    for record in &observed {
        let statement = rootlight_ir::decode_rdf_statement_envelope(record).unwrap();
        statement.verify_source_text(text).unwrap();
        assert_eq!(statement.object().literal_value(), Some("café"));
        assert!(statement.source().line_hint().is_some());
    }
    let exhausted = reader
        .extensions(
            observed.last().map(|record| record.id),
            GenerationReadLimit::new(2).unwrap(),
            &context,
        )
        .unwrap();
    assert!(exhausted.items().is_empty());
    assert_eq!(exhausted.total_available(), 5);
    assert_eq!(exhausted.completeness(), ReadPageCompleteness::Complete);
}

#[test]
fn rdf_partition_id_selection_leaves_unselected_payload_out_of_query_budget() {
    use crate::RdfPartitionWriter;
    use rootlight_ir::IrDocumentUsage;
    use rootlight_storage::VerifiedRdfPartition;
    let texts = [
        "_:a <urn:p> \"one\" .".to_owned(),
        format!("_:a <urn:p> \"{}\" .", "x".repeat(64 * 1024)),
        "_:a <urn:p> \"two\" .".to_owned(),
        "_:a <urn:p> \"three\" .".to_owned(),
    ];
    let source = texts.join("\n") + "\n";
    let (metadata, document) = rdf_partition_parent_document_for_source(
        1,
        content_hash(source.as_bytes()),
        u64::try_from(source.len()).unwrap(),
        b"rdf-id-selection",
    );
    let file = document.files[0].clone();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let limits = IrLimits::default();
    let parent = IdentityVerifiedGeneration::verify(
        metadata,
        document,
        &limits,
        &ExtensionSupport::default(),
        &context,
    )
    .unwrap();
    let mut records = rdf_partition_varied_records(metadata, &file, &texts);
    records.sort_by_key(|record| record.id);
    let large = records
        .iter()
        .max_by_key(|record| record.payload.len())
        .unwrap();
    let large_id = large.id;
    let large_bytes = large.payload.len();
    let large_index = records
        .iter()
        .position(|record| record.id == large_id)
        .unwrap();
    // Three small rows guarantee two adjacent small selections on one side of
    // the large row, without relying on the hash order of fixture identities.
    let after = if large_index >= 2 {
        None
    } else {
        Some(large_id)
    };
    let selected: Vec<_> = records
        .iter()
        .filter(|record| after.is_none_or(|cursor| record.id > cursor))
        .take(2)
        .cloned()
        .collect();
    assert_eq!(selected.len(), 2);
    let selected_ids: Vec<_> = selected.iter().map(|record| record.id).collect();
    let all_ids: Vec<_> = records.iter().map(|record| record.id).collect();
    let usage = selected
        .iter()
        .fold(IrDocumentUsage::default(), |usage, record| {
            usage
                .checked_add(
                    IrDocumentUsage::measure_extension(record, &limits).unwrap(),
                    &limits,
                )
                .unwrap()
        });
    let text_bytes =
        u64::try_from(usage.total_string_bytes() + usage.total_extension_bytes()).unwrap();
    assert!(u64::try_from(large_bytes).unwrap() > text_bytes);
    let directory = TempDir::new().unwrap();
    let partition =
        VerifiedRdfPartition::verify(&parent, file.id, records, &limits, &context).unwrap();
    let reader = RdfPartitionWriter::create_in(directory.path(), partition.descriptor(), &context)
        .unwrap()
        .seal(partition, &limits, &context)
        .unwrap();
    drop(parent);
    let metadata_only =
        GenerationContext::new(&cancellation, GenerationBudget::new(100, 1, 1).unwrap());
    assert_eq!(reader.select_ids(None, 4, &metadata_only).unwrap(), all_ids);
    let tight = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(2, 2, text_bytes).unwrap(),
    );
    assert_eq!(reader.select_ids(after, 2, &tight).unwrap(), selected_ids);
    assert_eq!(
        reader.read_selected(&selected_ids, &tight).unwrap(),
        selected
    );
    let page = reader
        .extensions(after, GenerationReadLimit::new(1).unwrap(), &tight)
        .unwrap();
    assert_eq!(page.items(), &selected[..1]);
    assert_eq!(page.total_available(), 4);
    assert_eq!(page.next_cursor(), Some(selected[0].id));
    assert!(matches!(
        reader
            .read_selected(&[large_id], &tight)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::BudgetExceeded {
            resource: GenerationResource::TextBytes,
            ..
        }
    ));
    assert!(reader.select_ids(None, 0, &context).is_err());
    assert!(
        reader
            .read_selected(&[selected_ids[0], selected_ids[0]], &context)
            .is_err()
    );
    cancellation.cancel(CancellationReason::ClientRequest);
    assert_eq!(
        reader.select_ids(None, 1, &context).unwrap_err().kind(),
        CatalogErrorKind::Cancelled
    );
    assert_eq!(
        reader
            .read_selected(&selected_ids, &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::Cancelled
    );
}

fn rdf_partition_varied_records(
    metadata: GenerationMetadata,
    file: &FileRecord,
    texts: &[String],
) -> Vec<rootlight_ir::ExtensionEnvelope> {
    let mut offset = 0u64;
    let records = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let predicate_start = text.find('<').unwrap();
            let predicate_end = text.find('>').unwrap() + 1;
            let span = |start, end| {
                SourceSpan::new(
                    file.id,
                    offset + u64::try_from(start).unwrap(),
                    offset + u64::try_from(end).unwrap(),
                )
                .unwrap()
            };
            let source = SourceRef::new(
                metadata.repository(),
                metadata.generation(),
                span(0, text.len()),
                file.content_hash,
                Some(
                    rootlight_ir::LineRange::new(
                        u64::try_from(index + 1).unwrap(),
                        u64::try_from(index + 1).unwrap(),
                    )
                    .unwrap(),
                ),
            );
            let statement = rootlight_ir::RdfStatement::from_ntriples(
                source,
                text,
                [
                    span(0, predicate_start - 1),
                    span(predicate_start, predicate_end),
                    span(predicate_end + 1, text.len() - 2),
                ],
            )
            .unwrap();
            offset += u64::try_from(text.len() + 1).unwrap();
            rootlight_ir::new_rdf_statement_envelope(file.provenance, &statement).unwrap()
        })
        .collect();
    assert_eq!(offset, file.byte_length);
    records
}

fn export_rdf_set_fixture_if_requested(
    directory: &Path,
    metadata: GenerationMetadata,
    descriptors: &[rootlight_storage::RdfPartitionDescriptor],
    context: &GenerationContext<'_>,
) {
    let Some(output) = std::env::var_os("ROOTLIGHT_RDF_SET_FIXTURE_DIRECTORY") else {
        return;
    };
    let output = PathBuf::from(output);
    assert!(output.is_absolute());
    fs::create_dir(&output).unwrap();
    let copy = |name: &str| {
        let target = output.join(name);
        create_private_test_file(&target);
        let mut input = fs::File::open(directory.join(name)).unwrap();
        let mut destination = fs::OpenOptions::new().write(true).open(target).unwrap();
        assert_eq!(
            std::io::copy(&mut input, &mut destination).unwrap(),
            input.metadata().unwrap().len()
        );
    };
    copy(ORACLE_FILENAME);
    for descriptor in descriptors {
        let name = format!("rdf-{}.sqlite3", descriptor.file());
        copy(&name);
    }
    drop(OracleReader::open_in(&output, context).unwrap());
    fs::write(output.join("fixture.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "repository": metadata.repository().to_string(),
        "generation": metadata.generation().to_string(),
        "files": descriptors.iter().map(|descriptor| descriptor.file().to_string()).collect::<Vec<_>>(),
    })).unwrap()).unwrap();
}

fn assert_rdf_descriptor_commitments(
    actual: &[rootlight_storage::RdfPartitionDescriptor],
    expected: &[rootlight_storage::RdfPartitionDescriptor],
) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.metadata(), expected.metadata());
        assert_eq!(actual.format_version(), expected.format_version());
        assert_eq!(actual.file(), expected.file());
        assert_eq!(actual.source_hash(), expected.source_hash());
        assert_eq!(actual.source_bytes(), expected.source_bytes());
        assert_eq!(actual.record_count(), expected.record_count());
        assert_eq!(actual.payload_bytes(), expected.payload_bytes());
        assert_eq!(actual.content_hash(), expected.content_hash());
    }
}

#[test]
fn rdf_set_pages_share_final_decode_budgets_and_skip_oversized_heads() {
    use rootlight_storage::{
        IdentityVerifiedRdfBase, SourceFileCatalog, VerifiedRdfPartition,
        new_rdf_partition_set_envelopes,
    };
    let texts = [
        "_:a <urn:p> \"one\" .".to_owned(),
        format!("_:a <urn:p> \"{}\" .", "x".repeat(64 * 1024)),
        "_:a <urn:p> \"two\" .".to_owned(),
        "_:a <urn:p> \"three\" .".to_owned(),
    ];
    let source = texts.join("\n") + "\n";
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let limits = IrLimits::default();
    let owner = |record: &rootlight_ir::ExtensionEnvelope| {
        (record.namespace == rootlight_ir::RDF_STATEMENT_NAMESPACE)
            .then(|| record.evidence.source.as_ref().unwrap().span().file())
    };
    // Vary only the declared configuration before writing any database, so
    // the regression does not depend on a particular stable-ID hash ordering.
    let (parent, document, expected, witness, file_witness) = (0..32)
        .find_map(|attempt| {
            let configuration = format!("rdf-set-page-budget-{attempt}");
            let (metadata, document) = rdf_partition_parent_document_for_source(
                2,
                content_hash(source.as_bytes()),
                u64::try_from(source.len()).unwrap(),
                configuration.as_bytes(),
            );
            let parent = IdentityVerifiedGeneration::verify(
                metadata,
                document,
                &limits,
                &ExtensionSupport::default(),
                &context,
            )
            .unwrap();
            let mut expected = Vec::new();
            let mut descriptors = Vec::new();
            for file in &parent.document().files {
                let records = rdf_partition_varied_records(metadata, file, &texts);
                expected.extend(records.clone());
                descriptors.push(
                    VerifiedRdfPartition::verify(&parent, file.id, records, &limits, &context)
                        .unwrap()
                        .descriptor()
                        .clone(),
                );
            }
            let mut document = parent.document().clone();
            document
                .extensions
                .extend(new_rdf_partition_set_envelopes(&parent, &descriptors, &context).unwrap());
            expected.extend(document.extensions.clone());
            expected.sort_by_key(|record| record.id);
            let witness = (0..expected.len() - 1).find_map(|start| {
                let after = start.checked_sub(1).map(|previous| expected[previous].id);
                let selected = &expected[start..start + 2];
                if selected
                    .iter()
                    .any(|record| record.payload.len() > 16 * 1024)
                    || owner(&selected[0]) == owner(&selected[1])
                    || selected[0].evidence.source == selected[1].evidence.source
                {
                    return None;
                }
                let oversized_head = expected.iter().find(|record| {
                    record.payload.len() > 16 * 1024
                        && record.id > selected[1].id
                        && !expected.iter().any(|earlier| {
                            owner(earlier) == owner(record)
                                && earlier.id < record.id
                                && after.is_none_or(|cursor| earlier.id > cursor)
                        })
                });
                oversized_head.map(|head| (after, selected.to_vec(), head.id))
            });
            let witness = witness?;
            let file_witness = parent.document().files.iter().find_map(|file| {
                let cursor = expected
                    .iter()
                    .filter(|record| {
                        owner(record).is_none()
                            && record
                                .evidence
                                .source
                                .as_ref()
                                .is_some_and(|source| source.span().file() == file.id)
                    })
                    .map(|record| record.id)
                    .max()?;
                let selected: Vec<_> = expected
                    .iter()
                    .filter(|record| owner(record) == Some(file.id) && record.id > cursor)
                    .take(2)
                    .cloned()
                    .collect();
                (selected.len() == 2).then_some((file.id, cursor, selected))
            })?;
            Some((parent, document, expected, witness, file_witness))
        })
        .expect(
            "bounded fixture search must establish oversized-head and all-leaf per-file witnesses",
        );
    let metadata = parent.metadata();
    let directory = TempDir::new().unwrap();
    for file in &parent.document().files {
        let partition = VerifiedRdfPartition::verify(
            &parent,
            file.id,
            rdf_partition_varied_records(metadata, file, &texts),
            &limits,
            &context,
        )
        .unwrap();
        drop(
            crate::RdfPartitionWriter::create_in(
                directory.path(),
                partition.descriptor(),
                &context,
            )
            .unwrap()
            .seal(partition, &limits, &context)
            .unwrap(),
        );
    }
    let base = IdentityVerifiedRdfBase::verify(
        metadata,
        document,
        SourceFileCatalog::default(),
        &limits,
        &context,
    )
    .unwrap();
    drop(parent);
    OracleWriter::create_in(directory.path())
        .unwrap()
        .write_rdf_base(base, &context)
        .unwrap();
    let reader = OracleReader::open_in(directory.path(), &context).unwrap();
    let (after, selected, large_id) = witness;
    let text_bytes = selected
        .iter()
        .map(|record| {
            u64::try_from(record.namespace.len() + record.version.len() + record.payload.len())
                .unwrap()
        })
        .sum::<u64>();
    assert!(text_bytes < 16 * 1024);
    let tight = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(100, 2, text_bytes).unwrap(),
    );
    let page = reader
        .extensions(after, GenerationReadLimit::new(1).unwrap(), &tight)
        .unwrap();
    assert_eq!(page.items(), &selected[..1]);
    assert_eq!(
        page.total_available(),
        u64::try_from(expected.len()).unwrap()
    );
    assert_eq!(page.next_cursor(), Some(selected[0].id));
    assert_eq!(page.completeness(), ReadPageCompleteness::Truncated);
    let text_short = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(100, 2, text_bytes - 1).unwrap(),
    );
    assert!(
        matches!(reader.extensions(after, GenerationReadLimit::new(1).unwrap(), &text_short).unwrap_err().kind(),
        CatalogErrorKind::BudgetExceeded { resource: GenerationResource::TextBytes, observed, limit }
        if observed == text_bytes && limit == text_bytes - 1)
    );
    let source_short = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(100, 1, text_bytes).unwrap(),
    );
    assert!(matches!(
        reader
            .extensions(after, GenerationReadLimit::new(1).unwrap(), &source_short)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::BudgetExceeded {
            resource: GenerationResource::SourceReferences,
            observed: 2,
            limit: 1
        }
    ));
    let before_large = expected
        .iter()
        .take_while(|record| record.id < large_id)
        .last()
        .map(|record| record.id);
    assert!(matches!(
        reader
            .extensions(before_large, GenerationReadLimit::new(1).unwrap(), &tight)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::BudgetExceeded {
            resource: GenerationResource::TextBytes,
            ..
        }
    ));
    let rows_short = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(1, 2, text_bytes).unwrap(),
    );
    assert!(matches!(
        reader
            .extensions(after, GenerationReadLimit::new(1).unwrap(), &rows_short)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::BudgetExceeded {
            resource: GenerationResource::Rows,
            ..
        }
    ));
    let (file, file_cursor, file_selected) = file_witness;
    let text_limit = GenerationBudget::default().limit(GenerationResource::TextBytes);
    let exact_sources = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(100, 3, text_limit).unwrap(),
    );
    let file_page = reader
        .extensions_for_file(
            file,
            Some(file_cursor),
            GenerationReadLimit::new(1).unwrap(),
            &exact_sources,
        )
        .unwrap()
        .unwrap();
    assert_eq!(file_page.items(), &file_selected[..1]);
    let two_sources = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(100, 2, text_limit).unwrap(),
    );
    assert!(matches!(
        reader
            .extensions_for_file(
                file,
                Some(file_cursor),
                GenerationReadLimit::new(1).unwrap(),
                &two_sources
            )
            .unwrap_err()
            .kind(),
        CatalogErrorKind::BudgetExceeded {
            resource: GenerationResource::SourceReferences,
            observed: 3,
            limit: 2
        }
    ));
    cancellation.cancel(CancellationReason::ClientRequest);
    assert_eq!(
        reader.read(&context).unwrap_err().kind(),
        CatalogErrorKind::Cancelled
    );
    assert_eq!(
        reader
            .extensions(after, GenerationReadLimit::new(1).unwrap(), &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::Cancelled
    );
}

#[test]
fn rdf_set_commitment_keeps_base_private_and_canonical_policy_strict() {
    use rootlight_storage::{
        IdentityVerifiedRdfBase, SourceFileCatalog, VerifiedRdfPartition,
        new_rdf_partition_set_envelopes, rdf_partition_extension_support,
    };
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let limits = IrLimits::default();
    let text = "_:a <urn:p> \"value\" .";
    let parent = rdf_partition_parent(2, text, 2, b"rdf-set-private-base");
    let directory = TempDir::new().unwrap();
    let mut censuses = Vec::new();
    let descriptors: Vec<_> = parent
        .document()
        .files
        .iter()
        .map(|file| {
            let partition = VerifiedRdfPartition::verify(
                &parent,
                file.id,
                rdf_partition_records(&parent, file.id, text, 1),
                &limits,
                &context,
            )
            .unwrap();
            let descriptor = partition.descriptor().clone();
            let reader =
                crate::RdfPartitionWriter::create_in(directory.path(), &descriptor, &context)
                    .unwrap()
                    .seal(partition, &limits, &context)
                    .unwrap();
            censuses.push(reader.census());
            descriptor
        })
        .collect();
    let mut document = parent.document().clone();
    document
        .extensions
        .extend(new_rdf_partition_set_envelopes(&parent, &descriptors, &context).unwrap());
    let mut expected_records = document.extensions.clone();
    for file in &parent.document().files {
        expected_records.extend(rdf_partition_records(&parent, file.id, text, 1));
    }
    expected_records.sort_by_key(|record| record.id);
    let metadata = parent.metadata();
    drop(parent);
    assert!(
        GenerationSnapshot::new(
            metadata,
            document.clone(),
            &limits,
            &ExtensionSupport::default()
        )
        .is_err()
    );
    assert_eq!(
        IdentityVerifiedGeneration::verify(
            metadata,
            document.clone(),
            &limits,
            &rdf_partition_extension_support(),
            &context
        )
        .unwrap_err(),
        IdentityVerificationError::UnsupportedExtension
    );
    assert_eq!(
        crate::write::measure_document(&document, &context, None)
            .err()
            .unwrap()
            .kind(),
        CatalogErrorKind::UnsupportedCriticalExtensions
    );
    let base = IdentityVerifiedRdfBase::verify(
        metadata,
        document,
        SourceFileCatalog::default(),
        &limits,
        &context,
    )
    .unwrap();
    assert_rdf_descriptor_commitments(base.partitions(), &descriptors);
    assert!(
        base.partitions()
            .iter()
            .all(|descriptor| descriptor.requires_parent_authentication())
    );
    assert!(
        descriptors
            .iter()
            .all(|descriptor| !descriptor.requires_parent_authentication())
    );
    OracleWriter::create_in(directory.path())
        .unwrap()
        .write_rdf_base(base, &context)
        .unwrap();
    let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
    let (_, stats, _) = crate::read::read_header(&connection, &context).unwrap();
    let cold = crate::read::read_rdf_base(&connection, metadata, stats, &context).unwrap();
    assert_rdf_descriptor_commitments(cold.partitions(), &descriptors);
    drop(cold);
    connection.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        crate::read::read_generation(&connection, metadata, stats, &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::PartitionedMaterializationRequired
    );
    let mut admitted = false;
    let result =
        crate::read::read_generation_with_admission(&connection, metadata, stats, &context, |_| {
            admitted = true;
            Ok::<(), ()>(())
        });
    assert!(
        matches!(result, Err(crate::OracleReadAdmissionError::Catalog(error))
        if error.kind() == CatalogErrorKind::PartitionedMaterializationRequired)
    );
    assert!(!admitted);
    drop(connection);
    let reader = OracleReader::open_in(directory.path(), &context).unwrap();
    let expected = censuses.into_iter().fold(stats, |stats, census| {
        census.checked_add_to(stats, 1).unwrap()
    });
    assert_eq!(reader.stats(), expected);
    assert_eq!(reader.stats().extensions(), stats.extensions() + 2);
    assert_eq!(reader.stats().source_refs(), stats.source_refs() + 2);
    let mut admitted = false;
    assert!(
        matches!(reader.read_with_admission(&context, |_| { admitted = true; Ok::<(), ()>(()) }),
        Err(crate::OracleReadAdmissionError::Catalog(error))
            if error.kind() == CatalogErrorKind::PartitionedMaterializationRequired)
    );
    assert!(!admitted);
    let disk_bytes = fs::metadata(directory.path().join(ORACLE_FILENAME))
        .unwrap()
        .len()
        + descriptors
            .iter()
            .map(|descriptor| {
                fs::metadata(
                    directory
                        .path()
                        .join(format!("rdf-{}.sqlite3", descriptor.file())),
                )
                .unwrap()
                .len()
            })
            .sum::<u64>();
    assert_eq!(reader.allocated_bytes(&context).unwrap(), disk_bytes);
    assert_eq!(
        reader.read(&context).unwrap_err().kind(),
        CatalogErrorKind::PartitionedMaterializationRequired
    );
    assert_eq!(
        reader.read_verified(&context).unwrap_err().kind(),
        CatalogErrorKind::PartitionedMaterializationRequired
    );
    assert_eq!(
        reader.read_generation(&context).unwrap_err().kind(),
        CatalogErrorKind::PartitionedMaterializationRequired
    );
    for file in [
        None,
        Some(descriptors[0].file()),
        Some(descriptors[1].file()),
    ] {
        let expected: Vec<_> = expected_records
            .iter()
            .filter(|record| {
                file.is_none_or(|file| {
                    record
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file)
                })
            })
            .cloned()
            .collect();
        let mut cursor = None;
        let mut observed = Vec::new();
        for _ in 0..=expected.len() {
            let page = if let Some(file) = file {
                reader
                    .extensions_for_file(
                        file,
                        cursor,
                        GenerationReadLimit::new(1).unwrap(),
                        &context,
                    )
                    .unwrap()
                    .unwrap()
            } else {
                reader
                    .extensions(cursor, GenerationReadLimit::new(1).unwrap(), &context)
                    .unwrap()
            };
            assert_eq!(
                page.total_available(),
                u64::try_from(expected.len()).unwrap()
            );
            observed.extend_from_slice(page.items());
            if page.completeness() == ReadPageCompleteness::Complete {
                break;
            }
            cursor = page.next_cursor();
            assert!(cursor.is_some());
        }
        assert_eq!(observed, expected);
        let cursor = expected.last().map(|record| record.id);
        let exhausted = if let Some(file) = file {
            reader
                .extensions_for_file(file, cursor, GenerationReadLimit::new(1).unwrap(), &context)
                .unwrap()
                .unwrap()
        } else {
            reader
                .extensions(cursor, GenerationReadLimit::new(1).unwrap(), &context)
                .unwrap()
        };
        assert!(exhausted.items().is_empty());
        assert_eq!(
            exhausted.total_available(),
            u64::try_from(expected.len()).unwrap()
        );
        assert_eq!(exhausted.completeness(), ReadPageCompleteness::Complete);
    }
    let missing_file = reader
        .extensions_for_file(
            FileId::from_bytes([0; 20]),
            None,
            GenerationReadLimit::new(1).unwrap(),
            &context,
        )
        .unwrap()
        .unwrap();
    assert!(missing_file.items().is_empty());
    assert_eq!(missing_file.total_available(), 0);
    drop(reader);
    let oracle_path = directory.path().join(ORACLE_FILENAME);
    assert_eq!(
        crate::schema::open_oracle_reader(&oracle_path, &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::ForeignDatabase
    );
    let database = Connection::open(&oracle_path).unwrap();
    let kind: Vec<u8> = database
        .query_row(
            "SELECT value FROM application_meta WHERE key = 'database_kind'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    database
        .execute(
            "UPDATE application_meta SET value = ?1 WHERE key = 'database_kind'",
            [b"rootlight-oracle".as_slice()],
        )
        .unwrap();
    assert_eq!(
        OracleReader::open_in(directory.path(), &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::Corrupt
    );
    database
        .execute(
            "UPDATE application_meta SET value = ?1 WHERE key = 'database_kind'",
            [b"rootlight-unknown".as_slice()],
        )
        .unwrap();
    assert_eq!(
        OracleReader::open_in(directory.path(), &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::ForeignDatabase
    );
    database
        .execute(
            "UPDATE application_meta SET value = ?1 WHERE key = 'database_kind'",
            [&kind],
        )
        .unwrap();
    let version: i64 = database
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    for invalid in [8, version + 1] {
        database
            .pragma_update(None, "user_version", invalid)
            .unwrap();
        assert_eq!(
            OracleReader::open_in(directory.path(), &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::IncompatibleSchema
        );
    }
    database
        .pragma_update(None, "user_version", version)
        .unwrap();
    drop(database);
    export_rdf_set_fixture_if_requested(directory.path(), metadata, &descriptors, &context);

    let canonical_directory = TempDir::new().unwrap();
    drop(
        OracleWriter::create_in(canonical_directory.path())
            .unwrap()
            .seal(
                rdf_partition_parent(1, text, 1, b"rdf-kind-without-set"),
                &context,
            )
            .unwrap(),
    );
    let database = Connection::open(canonical_directory.path().join(ORACLE_FILENAME)).unwrap();
    database
        .execute(
            "UPDATE application_meta SET value = ?1 WHERE key = 'database_kind'",
            [&kind],
        )
        .unwrap();
    drop(database);
    assert_eq!(
        OracleReader::open_in(canonical_directory.path(), &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::Corrupt
    );
    let first = directory
        .path()
        .join(format!("rdf-{}.sqlite3", descriptors[0].file()));
    let absent = directory.path().join("missing-member.sqlite3");
    fs::rename(&first, &absent).unwrap();
    assert_eq!(
        OracleReader::open_in(directory.path(), &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::NotFound
    );
    fs::rename(&absent, &first).unwrap();
    let database = Connection::open(&first).unwrap();
    let original: String = database
        .query_row("SELECT payload FROM rdf_statements", [], |row| row.get(0))
        .unwrap();
    database
        .execute("UPDATE rdf_statements SET payload = payload || ' '", [])
        .unwrap();
    drop(database);
    assert!(OracleReader::open_in(directory.path(), &context).is_err());
    let database = Connection::open(&first).unwrap();
    database
        .execute("UPDATE rdf_statements SET payload = ?1", [&original])
        .unwrap();
    drop(database);
    fs::copy(
        directory
            .path()
            .join(format!("rdf-{}.sqlite3", descriptors[1].file())),
        first,
    )
    .unwrap();
    assert!(OracleReader::open_in(directory.path(), &context).is_err());
}

#[test]
fn rdf_set_cold_authentication_rechecks_final_parent_provenance() {
    use rootlight_storage::{
        IdentityVerifiedRdfBase, SourceFileCatalog, VerifiedRdfPartition,
        new_rdf_partition_set_envelopes,
    };
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let limits = IrLimits::default();
    let text = "_:a <urn:p> \"value\" .";
    let parent = rdf_partition_parent(1, text, 1, b"rdf-final-parent-provenance");
    let metadata = parent.metadata();
    let file = parent.document().files[0].id;
    let mut alternate_document = parent.document().clone();
    let mut alternate_provenance = alternate_document.provenance[0].clone();
    alternate_provenance.producer =
        ProducerIdentity::new("catalog-alternate-rdf", "1", metadata.configuration_hash()).unwrap();
    alternate_provenance.id = derive_provenance_record_id(&alternate_provenance).unwrap();
    alternate_document
        .provenance
        .push(alternate_provenance.clone());
    let alternate_parent = IdentityVerifiedGeneration::verify(
        metadata,
        alternate_document,
        &limits,
        &ExtensionSupport::default(),
        &context,
    )
    .unwrap();
    let records = rdf_partition_records(&parent, file, text, 1)
        .iter()
        .map(|record| {
            rootlight_ir::new_rdf_statement_envelope(
                alternate_provenance.id,
                &rootlight_ir::decode_rdf_statement_envelope(record).unwrap(),
            )
            .unwrap()
        })
        .collect();
    let partition =
        VerifiedRdfPartition::verify(&alternate_parent, file, records, &limits, &context).unwrap();
    let descriptor = partition.descriptor().clone();
    let directory = TempDir::new().unwrap();
    drop(
        crate::RdfPartitionWriter::create_in(directory.path(), &descriptor, &context)
            .unwrap()
            .seal(partition, &limits, &context)
            .unwrap(),
    );
    let mut document = parent.document().clone();
    document
        .extensions
        .extend(new_rdf_partition_set_envelopes(&parent, &[descriptor], &context).unwrap());
    let base = IdentityVerifiedRdfBase::verify(
        metadata,
        document,
        SourceFileCatalog::default(),
        &limits,
        &context,
    )
    .unwrap();
    assert_eq!(
        crate::RdfPartitionReader::open_in(
            directory.path(),
            &base.partitions()[0],
            &limits,
            &context
        )
        .unwrap_err()
        .kind(),
        CatalogErrorKind::IdentityProofRequired,
    );
    drop(parent);
    drop(alternate_parent);
    OracleWriter::create_in(directory.path())
        .unwrap()
        .write_rdf_base(base, &context)
        .unwrap();
    assert_eq!(
        OracleReader::open_in(directory.path(), &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::Corrupt
    );
}

#[test]
fn rdf_set_commitment_rejects_missing_mixed_unknown_and_inline_members() {
    use rootlight_storage::{
        IdentityVerifiedRdfBase, RDF_PARTITION_SET_NAMESPACE, SourceFileCatalog,
        VerifiedRdfPartition, new_rdf_partition_set_envelopes,
    };
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let limits = IrLimits::default();
    let text = "_:a <urn:p> \"value\" .";
    let parent = rdf_partition_parent(2, text, 2, b"rdf-set-negatives");
    let descriptors: Vec<_> = parent
        .document()
        .files
        .iter()
        .map(|file| {
            VerifiedRdfPartition::verify(
                &parent,
                file.id,
                rdf_partition_records(&parent, file.id, text, 1),
                &limits,
                &context,
            )
            .unwrap()
            .descriptor()
            .clone()
        })
        .collect();
    let envelopes = new_rdf_partition_set_envelopes(&parent, &descriptors, &context).unwrap();
    let mut document = parent.document().clone();
    document.extensions.extend(envelopes.clone());
    let root_id = envelopes[0].id;
    let member_id = envelopes[1].id;
    let verify = |document| {
        IdentityVerifiedRdfBase::verify(
            parent.metadata(),
            document,
            SourceFileCatalog::default(),
            &limits,
            &context,
        )
    };
    for missing in [root_id, member_id] {
        let mut bad = document.clone();
        bad.extensions.retain(|record| record.id != missing);
        assert!(verify(bad).is_err());
    }
    for (field, value) in [
        ("version", "2"),
        ("namespace", "rootlight.unknown_critical"),
    ] {
        let mut bad = document.clone();
        let root = bad
            .extensions
            .iter_mut()
            .find(|record| record.id == root_id)
            .unwrap();
        if field == "version" {
            root.version = value.into();
        } else {
            root.namespace = value.into();
        }
        assert!(verify(bad).is_err());
    }
    for payload in [
        r#"{"kind":"unknown"}"#,
        r#"{"kind":"root","members":2,"set_hash":"invalid","extra":1}"#,
    ] {
        let mut bad = document.clone();
        bad.extensions
            .iter_mut()
            .find(|record| record.id == root_id)
            .unwrap()
            .payload = payload.into();
        assert!(verify(bad).is_err());
    }
    let file = parent.document().files[0].id;
    let alternate = VerifiedRdfPartition::verify(
        &parent,
        file,
        rdf_partition_records(&parent, file, text, 2),
        &limits,
        &context,
    )
    .unwrap();
    let alternate_set =
        new_rdf_partition_set_envelopes(&parent, &[alternate.descriptor().clone()], &context)
            .unwrap();
    let alternate_member = alternate_set
        .into_iter()
        .find(|record| {
            serde_json::from_str::<serde_json::Value>(&record.payload).unwrap()["kind"] == "member"
        })
        .unwrap();
    let mut mixed = document.clone();
    mixed.extensions.retain(|record| record.id != member_id);
    mixed.extensions.push(alternate_member.clone());
    assert!(verify(mixed).is_err());
    let mut duplicate = document.clone();
    duplicate.extensions.push(alternate_member);
    assert!(verify(duplicate).is_err());
    let mut inline = document.clone();
    inline
        .extensions
        .extend(rdf_partition_records(&parent, file, text, 1));
    assert!(verify(inline).is_err());
    let tight = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(2, 100, 100_000).unwrap(),
    );
    assert!(matches!(
        new_rdf_partition_set_envelopes(&parent, &descriptors, &tight),
        Err(rootlight_storage::RdfPartitionError::Control(_))
    ));
    let stopped = Cancellation::new();
    stopped.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        IdentityVerifiedRdfBase::verify(
            parent.metadata(),
            document,
            SourceFileCatalog::default(),
            &limits,
            &default_context(&stopped)
        ),
        Err(IdentityVerificationError::Control(_))
    ));
    assert!(
        envelopes
            .iter()
            .all(|record| record.namespace == RDF_PARTITION_SET_NAMESPACE
                && record.criticality == ExtensionCriticality::Critical
                && record.payload.len() <= 4096)
    );
}

#[test]
fn packed_rdf_census_matches_physical_utf8_storage_and_distinct_sources() {
    use crate::{RdfPartitionReader, RdfPartitionWriter};
    use rootlight_storage::{GenerationStatsLayout, VerifiedRdfPartition};
    let text = "_:a <urn:p> \"café\" .";
    let (metadata, mut document) =
        rdf_partition_parent_document(1, text, 1, b"packed-physical-census");
    let mut second = document.provenance[0].clone();
    second.producer =
        ProducerIdentity::new("catalog-rdf-second", "1", content_hash(b"second-producer")).unwrap();
    second.id = derive_provenance_record_id(&second).unwrap();
    let second_id = second.id;
    document.provenance.push(second);
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let limits = IrLimits::default();
    let parent = IdentityVerifiedGeneration::verify(
        metadata,
        document,
        &limits,
        &ExtensionSupport::default(),
        &context,
    )
    .unwrap();
    let base = crate::write::measure(parent.snapshot(), &context)
        .unwrap()
        .stats;
    let file = parent.document().files[0].id;
    let first = rdf_partition_records(&parent, file, text, 1).pop().unwrap();
    let statement = rootlight_ir::decode_rdf_statement_envelope(&first).unwrap();
    let second = rootlight_ir::new_rdf_statement_envelope(second_id, &statement).unwrap();
    assert_eq!(first.evidence.source, second.evidence.source);
    let partition =
        VerifiedRdfPartition::verify(&parent, file, vec![first, second], &limits, &context)
            .unwrap();
    let descriptor = partition.descriptor().clone();
    let directory = TempDir::new().unwrap();
    drop(
        RdfPartitionWriter::create_in(directory.path(), &descriptor, &context)
            .unwrap()
            .seal(partition, &limits, &context)
            .unwrap(),
    );
    let one_source = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(100, 1, 100_000).unwrap(),
    );
    let reader =
        RdfPartitionReader::open_in(directory.path(), &descriptor, &limits, &one_source).unwrap();
    let census = reader.census();
    assert_eq!(census.statements(), 2);
    assert_eq!(census.distinct_source_refs(), 1);
    let database = Connection::open(directory.path().join(format!("rdf-{file}.sqlite3"))).unwrap();
    let (rows, source_bytes, payload_bytes, characters): (i64,i64,i64,i64) = database.query_row(
        "SELECT count(*), sum(length(source)), sum(length(CAST(payload AS BLOB))), sum(length(payload)) FROM rdf_statements", [],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    let (headers, header_bytes): (i64, i64) = database
        .query_row(
            "SELECT count(*), sum(length(header)) FROM partition_descriptor",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let [
        rows,
        source_bytes,
        payload_bytes,
        characters,
        headers,
        header_bytes,
    ] = [
        rows,
        source_bytes,
        payload_bytes,
        characters,
        headers,
        header_bytes,
    ]
    .map(|value| u64::try_from(value).unwrap());
    assert!(payload_bytes > characters);
    assert_eq!(census.source_json_bytes(), source_bytes);
    assert_eq!(census.payload_utf8_bytes(), payload_bytes);
    assert_eq!(census.descriptor_json_bytes(), header_bytes);
    let packed = census.checked_add_to(base, 1).unwrap();
    assert_eq!(
        packed.layout(),
        GenerationStatsLayout::CanonicalOracleWithRdfV1
    );
    assert_eq!(packed.extensions(), base.extensions() + rows);
    assert_eq!(packed.source_refs(), base.source_refs() + 1);
    assert_eq!(packed.stored_rows(), base.stored_rows() + rows + headers);
    assert_eq!(
        packed.text_bytes(),
        base.text_bytes() + source_bytes + payload_bytes + header_bytes
    );
    assert!(census.checked_add_to(base, 2).is_err());
    let page = reader
        .extensions(None, GenerationReadLimit::new(1).unwrap(), &one_source)
        .unwrap();
    assert_eq!(page.items().len(), 1);
    assert_eq!(page.total_available(), 2);
    assert_eq!(page.completeness(), ReadPageCompleteness::Truncated);
}

#[test]
fn unpublished_rdf_stage_drops_input_before_full_parent_and_cold_eof() {
    use crate::{RdfPartitionReader, RdfPartitionStage};
    use rootlight_storage::{UnpublishedRdfPartition, VerifiedRdfPartition};
    let text = "_:a <urn:p> \"caf\\u00E9\"@en .";
    let (metadata, document) = rdf_partition_parent_document(2, text, 5, b"unpublished-rdf");
    let file = document.files[0].clone();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let limits = IrLimits::default();
    let records = rdf_partition_records_claimed(metadata, &file, text, 5);
    let partition = UnpublishedRdfPartition::admit(
        metadata,
        file.id,
        file.content_hash,
        file.byte_length,
        records,
        &limits,
        &context,
    )
    .unwrap();
    let estimate = RdfPartitionStage::storage_estimate(&partition).unwrap();
    assert!(estimate.admit(estimate.required_bytes() - 1).is_err());
    let reservation = estimate.admit(estimate.required_bytes()).unwrap();
    let directory = TempDir::new().unwrap();
    // No verified parent exists while the original owned payload is consumed.
    let stage =
        RdfPartitionStage::write_in(directory.path(), partition, reservation, &context).unwrap();
    let path = directory.path().join(format!("rdf-{}.sqlite3", file.id));
    assert!(fs::metadata(&path).unwrap().len() <= reservation.final_bytes());
    let database = Connection::open(&path).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM partition_descriptor", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM rdf_statements", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        5
    );
    drop(database);
    let parent = IdentityVerifiedGeneration::verify(
        metadata,
        document,
        &limits,
        &ExtensionSupport::default(),
        &context,
    )
    .unwrap();
    assert_eq!(parent.document().files.len(), 2);
    let expected = VerifiedRdfPartition::verify(
        &parent,
        file.id,
        rdf_partition_records_claimed(metadata, &file, text, 5),
        &limits,
        &context,
    )
    .unwrap();
    let descriptor = expected.descriptor().clone();
    let expected = expected.records().to_vec();
    // Even an independently authenticated descriptor cannot read an unfinished stage.
    assert!(RdfPartitionReader::open_in(directory.path(), &descriptor, &limits, &context).is_err());
    let reader = stage.authenticate(&parent, &limits, &context).unwrap();
    assert_eq!(reader.descriptor(), &descriptor);
    drop(reader);
    drop(parent);
    let reader =
        RdfPartitionReader::open_in(directory.path(), &descriptor, &limits, &context).unwrap();
    let mut observed = Vec::new();
    let mut cursor = None;
    loop {
        let page = reader
            .extensions(cursor, GenerationReadLimit::new(2).unwrap(), &context)
            .unwrap();
        assert_eq!(page.total_available(), 5);
        let next = page.next_cursor();
        observed.extend(page.into_items());
        if next.is_none() {
            break;
        }
        cursor = next;
    }
    assert_eq!(observed, expected);
    for record in observed {
        let statement = rootlight_ir::decode_rdf_statement_envelope(&record).unwrap();
        statement.verify_source_text(text).unwrap();
        assert_eq!(statement.object().literal_value(), Some("café"));
        assert!(statement.source().line_hint().is_some());
    }
}

#[test]
fn unpublished_rdf_stage_rejects_identity_quota_and_unreserved_writes() {
    use crate::RdfPartitionStage;
    use rootlight_storage::{PublicationSpaceEstimate, RdfPartitionError, UnpublishedRdfPartition};
    let text = "_:a <urn:p> \"value\" .";
    let (metadata, document) = rdf_partition_parent_document(2, text, 2, b"unpublished-admission");
    let file = &document.files[0];
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let limits = IrLimits::default();
    let records = rdf_partition_records_claimed(metadata, file, text, 2);
    let mut forged = records.clone();
    forged[0].id = FactId::from_bytes([255; 20]);
    assert!(matches!(
        UnpublishedRdfPartition::admit(
            metadata,
            file.id,
            file.content_hash,
            file.byte_length,
            forged,
            &limits,
            &context
        ),
        Err(RdfPartitionError::InvalidRecord)
    ));
    let other = &document.files[1];
    assert!(matches!(
        UnpublishedRdfPartition::admit(
            metadata,
            other.id,
            other.content_hash,
            other.byte_length,
            records.clone(),
            &limits,
            &context
        ),
        Err(RdfPartitionError::InvalidRecord)
    ));
    let bytes = records.iter().map(|record| record.payload.len()).sum();
    let mut exact = limits.clone();
    exact.max_total_extension_bytes = bytes;
    UnpublishedRdfPartition::admit(
        metadata,
        file.id,
        file.content_hash,
        file.byte_length,
        records.clone(),
        &exact,
        &context,
    )
    .unwrap();
    exact.max_total_extension_bytes -= 1;
    assert!(matches!(
        UnpublishedRdfPartition::admit(
            metadata,
            file.id,
            file.content_hash,
            file.byte_length,
            records.clone(),
            &exact,
            &context
        ),
        Err(RdfPartitionError::Quota(_))
    ));
    let partition = UnpublishedRdfPartition::admit(
        metadata,
        file.id,
        file.content_hash,
        file.byte_length,
        records.clone(),
        &limits,
        &context,
    )
    .unwrap();
    let directory = TempDir::new().unwrap();
    let insufficient = PublicationSpaceEstimate::new(1, 0, 0)
        .unwrap()
        .admit(1)
        .unwrap();
    assert!(
        RdfPartitionStage::write_in(directory.path(), partition, insufficient, &context).is_err()
    );
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    for (resource, rows, sources, text) in [
        (GenerationResource::Rows, 1, 2, 100_000),
        (GenerationResource::SourceReferences, 2, 1, 100_000),
        (GenerationResource::TextBytes, 2, 2, 1),
    ] {
        let partition = UnpublishedRdfPartition::admit(
            metadata,
            file.id,
            file.content_hash,
            file.byte_length,
            records.clone(),
            &limits,
            &context,
        )
        .unwrap();
        let estimate = RdfPartitionStage::storage_estimate(&partition).unwrap();
        let reservation = estimate.admit(estimate.required_bytes()).unwrap();
        let constrained = GenerationContext::new(
            &cancellation,
            GenerationBudget::new(rows, sources, text).unwrap(),
        );
        let error =
            RdfPartitionStage::write_in(directory.path(), partition, reservation, &constrained)
                .unwrap_err();
        assert!(
            matches!(error.kind(), CatalogErrorKind::BudgetExceeded { resource: actual, .. } if actual == resource)
        );
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        UnpublishedRdfPartition::admit(
            metadata,
            file.id,
            file.content_hash,
            file.byte_length,
            records,
            &limits,
            &context
        ),
        Err(RdfPartitionError::Control(_))
    ));
}

#[test]
fn unpublished_rdf_stage_final_authentication_rejects_drift_and_cancellation() {
    use crate::RdfPartitionStage;
    use rootlight_storage::UnpublishedRdfPartition;
    let text = "_:a <urn:p> \"value\" .";
    let parent = rdf_partition_parent(2, text, 2, b"unpublished-final");
    let foreign = rdf_partition_parent(2, text, 2, b"foreign-unpublished-final");
    let file = &parent.document().files[0];
    let limits = IrLimits::default();
    for fault in [
        "provenance",
        "foreign_parent",
        "payload",
        "count",
        "header",
        "quota",
        "cancel",
    ] {
        let cancellation = Cancellation::new();
        let context = default_context(&cancellation);
        let mut records = rdf_partition_records(&parent, file.id, text, 2);
        if fault == "provenance" {
            let statement = rootlight_ir::decode_rdf_statement_envelope(&records[0]).unwrap();
            records[0] =
                rootlight_ir::new_rdf_statement_envelope(FactId::from_bytes([254; 20]), &statement)
                    .unwrap();
        }
        let partition = UnpublishedRdfPartition::admit(
            parent.metadata(),
            file.id,
            file.content_hash,
            file.byte_length,
            records,
            &limits,
            &context,
        )
        .unwrap();
        let reservation = RdfPartitionStage::storage_estimate(&partition)
            .unwrap()
            .admit(u64::MAX)
            .unwrap();
        let directory = TempDir::new().unwrap();
        let stage = RdfPartitionStage::write_in(directory.path(), partition, reservation, &context)
            .unwrap();
        let path = directory.path().join(format!("rdf-{}.sqlite3", file.id));
        let database = Connection::open(&path).unwrap();
        match fault {
            "payload" => {
                database
                    .execute("UPDATE rdf_statements SET payload = payload || ' '", [])
                    .unwrap();
            }
            "count" => {
                database.execute("DELETE FROM rdf_statements WHERE record_id = (SELECT min(record_id) FROM rdf_statements)", []).unwrap();
            }
            "header" => {
                database
                    .execute(
                        "INSERT INTO partition_descriptor(singleton, header) VALUES(1, X'00')",
                        [],
                    )
                    .unwrap();
            }
            _ => {}
        }
        drop(database);
        let mut read_limits = limits.clone();
        if fault == "quota" {
            read_limits.max_total_extension_bytes = 1;
        }
        if fault == "cancel" {
            cancellation.cancel(CancellationReason::ClientRequest);
        }
        let selected_parent = if fault == "foreign_parent" {
            &foreign
        } else {
            &parent
        };
        let error = stage
            .authenticate(selected_parent, &read_limits, &context)
            .unwrap_err();
        if fault == "cancel" {
            assert_eq!(error.kind(), CatalogErrorKind::Cancelled);
        }
        let database = Connection::open(&path).unwrap();
        let headers: i64 = database
            .query_row("SELECT count(*) FROM partition_descriptor", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(headers, i64::from(fault == "header"), "{fault}");
    }
}

#[test]
fn rdf_partition_admission_rejects_identity_overlap_and_original_quotas() {
    use rootlight_storage::{RdfPartitionError, VerifiedRdfPartition};
    let text = "_:a <urn:p> \"value\" .";
    let parent = rdf_partition_parent(2, text, 2, b"rdf-admission");
    let file = parent.document().files[0].id;
    let records = rdf_partition_records(&parent, file, text, 2);
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let limits = IrLimits::default();
    assert!(matches!(
        VerifiedRdfPartition::verify(
            &parent,
            FileId::from_bytes([255; 20]),
            records.clone(),
            &limits,
            &context
        ),
        Err(RdfPartitionError::MissingFile)
    ));
    assert!(matches!(
        VerifiedRdfPartition::verify(
            &parent,
            parent.document().files[1].id,
            records.clone(),
            &limits,
            &context
        ),
        Err(RdfPartitionError::InvalidRecord)
    ));
    assert!(matches!(
        VerifiedRdfPartition::verify(
            &parent,
            file,
            vec![records[0].clone(), records[0].clone()],
            &limits,
            &context
        ),
        Err(RdfPartitionError::DuplicateIdentity)
    ));
    let foreign = rdf_partition_parent(2, text, 2, b"foreign-rdf-admission");
    assert!(matches!(
        VerifiedRdfPartition::verify(&foreign, file, records.clone(), &limits, &context),
        Err(RdfPartitionError::InvalidRecord)
    ));
    let mut forged = records.clone();
    forged[0].payload.push(' ');
    assert!(matches!(
        VerifiedRdfPartition::verify(&parent, file, forged, &limits, &context),
        Err(RdfPartitionError::InvalidRecord)
    ));
    let statement = rootlight_ir::decode_rdf_statement_envelope(&records[0]).unwrap();
    let original_source = statement.source();
    let wrong_hash = SourceRef::new(
        original_source.repository(),
        original_source.generation(),
        original_source.span(),
        content_hash(b"different immutable source"),
        original_source.line_hint(),
    );
    let wrong_statement = rootlight_ir::RdfStatement::from_ntriples(
        wrong_hash,
        text,
        [
            statement.subject().source().span(),
            statement.predicate().source().span(),
            statement.object().source().span(),
        ],
    )
    .unwrap();
    let wrong_hash =
        rootlight_ir::new_rdf_statement_envelope(records[0].provenance, &wrong_statement).unwrap();
    assert!(matches!(
        VerifiedRdfPartition::verify(&parent, file, vec![wrong_hash], &limits, &context),
        Err(RdfPartitionError::InvalidRecord)
    ));
    let unknown =
        rootlight_ir::new_rdf_statement_envelope(FactId::from_bytes([254; 20]), &statement)
            .unwrap();
    assert!(matches!(
        VerifiedRdfPartition::verify(&parent, file, vec![unknown], &limits, &context),
        Err(RdfPartitionError::UnknownProvenance)
    ));
    let bytes = records.iter().map(|record| record.payload.len()).sum();
    let mut exact = limits.clone();
    exact.max_total_extension_bytes = bytes;
    VerifiedRdfPartition::verify(&parent, file, records.clone(), &exact, &context).unwrap();
    exact.max_total_extension_bytes -= 1;
    assert!(matches!(
        VerifiedRdfPartition::verify(&parent, file, records.clone(), &exact, &context),
        Err(RdfPartitionError::Quota(_))
    ));
    let mut inline = parent.document().clone();
    inline.extensions.extend(records.clone());
    let inline = IdentityVerifiedGeneration::verify(
        parent.metadata(),
        inline,
        &limits,
        &ExtensionSupport::default(),
        &context,
    )
    .unwrap();
    assert!(matches!(
        VerifiedRdfPartition::verify(&inline, file, records.clone(), &limits, &context),
        Err(RdfPartitionError::OverlappingFile)
    ));
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        VerifiedRdfPartition::verify(&parent, file, records, &limits, &context),
        Err(RdfPartitionError::Control(_))
    ));
}

#[test]
fn rdf_partition_cold_open_rejects_tampering_and_preserves_control_errors() {
    use crate::{RdfPartitionReader, RdfPartitionWriter};
    use rootlight_storage::VerifiedRdfPartition;
    let text = "_:a <urn:p> \"value\" .";
    let parent = rdf_partition_parent(1, text, 2, b"rdf-tamper");
    let file = parent.document().files[0].id;
    let limits = IrLimits::default();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    for mutation in ["header", "count", "payload", "provenance", "source"] {
        let directory = TempDir::new().unwrap();
        let partition = VerifiedRdfPartition::verify(
            &parent,
            file,
            rdf_partition_records(&parent, file, text, 2),
            &limits,
            &context,
        )
        .unwrap();
        let descriptor = partition.descriptor().clone();
        drop(
            RdfPartitionWriter::create_in(directory.path(), &descriptor, &context)
                .unwrap()
                .seal(partition, &limits, &context)
                .unwrap(),
        );
        let path = directory.path().join(format!("rdf-{file}.sqlite3"));
        let database = Connection::open(&path).unwrap();
        match mutation {
            "header" => {
                database
                    .execute("UPDATE partition_descriptor SET header = X'00'", [])
                    .unwrap();
            }
            "count" => {
                database.execute("DELETE FROM rdf_statements WHERE record_id = (SELECT min(record_id) FROM rdf_statements)", []).unwrap();
            }
            "payload" => {
                database
                    .execute("UPDATE rdf_statements SET payload = payload || ' '", [])
                    .unwrap();
            }
            "provenance" => {
                database
                    .execute("UPDATE rdf_statements SET provenance_id = zeroblob(20)", [])
                    .unwrap();
            }
            "source" => {
                database
                    .execute("UPDATE rdf_statements SET source = X'7b7d'", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        drop(database);
        assert!(
            RdfPartitionReader::open_in(directory.path(), &descriptor, &limits, &context).is_err(),
            "{mutation}"
        );
    }
    let directory = TempDir::new().unwrap();
    let partition = VerifiedRdfPartition::verify(
        &parent,
        file,
        rdf_partition_records(&parent, file, text, 2),
        &limits,
        &context,
    )
    .unwrap();
    let descriptor = partition.descriptor().clone();
    drop(
        RdfPartitionWriter::create_in(directory.path(), &descriptor, &context)
            .unwrap()
            .seal(partition, &limits, &context)
            .unwrap(),
    );
    let bounded =
        GenerationContext::new(&cancellation, GenerationBudget::new(100, 100, 1).unwrap());
    assert!(matches!(
        RdfPartitionReader::open_in(directory.path(), &descriptor, &limits, &bounded)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::BudgetExceeded {
            resource: GenerationResource::EncodedTextBytes,
            ..
        }
    ));
    cancellation.cancel(CancellationReason::ClientRequest);
    assert_eq!(
        RdfPartitionReader::open_in(directory.path(), &descriptor, &limits, &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::Cancelled
    );
}

#[test]
fn rdf_partition_empty_commitment_and_cancelled_stage_remain_fail_closed() {
    use crate::{RdfPartitionReader, RdfPartitionWriter};
    use rootlight_storage::VerifiedRdfPartition;
    let parent = rdf_partition_parent(1, "", 0, b"empty-rdf");
    let foreign = rdf_partition_parent(1, "", 0, b"foreign-empty-rdf");
    let file = parent.document().files[0].id;
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let limits = IrLimits::default();
    let partition =
        VerifiedRdfPartition::verify(&parent, file, Vec::new(), &limits, &context).unwrap();
    let descriptor = partition.descriptor().clone();
    assert_eq!(
        descriptor.format_version(),
        rootlight_storage::RDF_PARTITION_FORMAT_VERSION
    );
    assert_eq!(descriptor.record_count(), 0);
    assert_eq!(descriptor.payload_bytes(), 0);
    let directory = TempDir::new().unwrap();
    drop(
        RdfPartitionWriter::create_in(directory.path(), &descriptor, &context)
            .unwrap()
            .seal(partition, &limits, &context)
            .unwrap(),
    );
    let foreign =
        VerifiedRdfPartition::verify(&foreign, file, Vec::new(), &limits, &context).unwrap();
    assert_eq!(
        descriptor.content_hash(),
        foreign.descriptor().content_hash()
    );
    assert!(
        RdfPartitionReader::open_in(directory.path(), foreign.descriptor(), &limits, &context)
            .is_err()
    );
    let reader =
        RdfPartitionReader::open_in(directory.path(), &descriptor, &limits, &context).unwrap();
    let empty = reader
        .extensions(None, GenerationReadLimit::new(1).unwrap(), &context)
        .unwrap();
    assert!(empty.items().is_empty());
    assert_eq!(empty.completeness(), ReadPageCompleteness::Complete);
    drop(reader);
    assert_eq!(
        RdfPartitionWriter::create_in(directory.path(), &descriptor, &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::AlreadyExists
    );
    let stage = TempDir::new().unwrap();
    let partition =
        VerifiedRdfPartition::verify(&parent, file, Vec::new(), &limits, &context).unwrap();
    let writer = RdfPartitionWriter::create_in(stage.path(), &descriptor, &context).unwrap();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert_eq!(
        writer
            .seal(partition, &limits, &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::Cancelled
    );
    let database = Connection::open(stage.path().join(format!("rdf-{file}.sqlite3"))).unwrap();
    let rows: i64 = database
        .query_row("SELECT count(*) FROM partition_descriptor", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);
    let rows: i64 = database
        .query_row("SELECT count(*) FROM rdf_statements", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn rdf_partition_pages_enforce_exact_source_and_metadata_text_budgets() {
    use crate::RdfPartitionWriter;
    use rootlight_ir::IrDocumentUsage;
    use rootlight_storage::VerifiedRdfPartition;
    let text = "_:a <urn:p> \"value\" .";
    let parent = rdf_partition_parent(1, text, 2, b"rdf-page-budgets");
    let file = parent.document().files[0].id;
    let limits = IrLimits::default();
    let records = rdf_partition_records(&parent, file, text, 2);
    let usage = records
        .iter()
        .fold(IrDocumentUsage::default(), |usage, record| {
            usage
                .checked_add(
                    IrDocumentUsage::measure_extension(record, &limits).unwrap(),
                    &limits,
                )
                .unwrap()
        });
    assert!(usage.total_string_bytes() > 0);
    let payload_bytes = u64::try_from(usage.total_extension_bytes()).unwrap();
    let text_bytes = payload_bytes + u64::try_from(usage.total_string_bytes()).unwrap();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let partition =
        VerifiedRdfPartition::verify(&parent, file, records, &limits, &context).unwrap();
    let directory = TempDir::new().unwrap();
    let reader = RdfPartitionWriter::create_in(directory.path(), partition.descriptor(), &context)
        .unwrap()
        .seal(partition, &limits, &context)
        .unwrap();
    let limit = GenerationReadLimit::new(1).unwrap();
    let exact = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(2, 2, text_bytes).unwrap(),
    );
    let page = reader.extensions(None, limit, &exact).unwrap();
    assert_eq!(page.items().len(), 1);
    assert_eq!(page.completeness(), ReadPageCompleteness::Truncated);
    let short_sources = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(2, 1, text_bytes).unwrap(),
    );
    assert!(matches!(
        reader
            .extensions(None, limit, &short_sources)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::BudgetExceeded {
            resource: GenerationResource::SourceReferences,
            observed: 2,
            limit: 1
        }
    ));
    let short_text = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(2, 2, payload_bytes).unwrap(),
    );
    assert!(
        matches!(reader.extensions(None, limit, &short_text).unwrap_err().kind(),
        CatalogErrorKind::BudgetExceeded { resource: GenerationResource::TextBytes, observed, limit } if observed == text_bytes && limit == payload_bytes)
    );
}

#[test]
fn rdf_partitions_cold_read_beyond_the_production_owned_payload_limit() {
    use crate::{RdfPartitionStage, RdfPartitionWriter};
    use rootlight_ir::IrDocumentUsage;
    use rootlight_storage::{
        IdentityVerifiedRdfBase, SourceFileCatalog, UnpublishedRdfPartition, VerifiedRdfPartition,
        new_rdf_partition_set_envelopes,
    };
    let text = format!("_:a <urn:p> \"{}\" .", "x".repeat(256 * 1024));
    let count = 260;
    let limits = IrLimits::default();
    let parent = rdf_partition_parent(2, &text, count, b"rdf-production-capacity");
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let directory = TempDir::new().unwrap();
    let mut descriptors = Vec::new();
    let mut summaries = Vec::new();
    for (index, file) in parent.document().files.iter().enumerate() {
        // Each path releases its owned payload before cold validation or the
        // next file. The unpublished path reloads only its admitted file.
        let records = rdf_partition_records(&parent, file.id, &text, count);
        let usage = records
            .iter()
            .fold(IrDocumentUsage::default(), |usage, record| {
                usage
                    .checked_add(
                        IrDocumentUsage::measure_extension(record, &limits).unwrap(),
                        &limits,
                    )
                    .unwrap()
            });
        let reader = if index == 0 {
            let partition =
                VerifiedRdfPartition::verify(&parent, file.id, records, &limits, &context).unwrap();
            RdfPartitionWriter::create_in(directory.path(), partition.descriptor(), &context)
                .unwrap()
                .seal(partition, &limits, &context)
                .unwrap()
        } else {
            let partition = UnpublishedRdfPartition::admit(
                parent.metadata(),
                file.id,
                file.content_hash,
                file.byte_length,
                records,
                &limits,
                &context,
            )
            .unwrap();
            let estimate = RdfPartitionStage::storage_estimate(&partition).unwrap();
            let reservation = estimate.admit(estimate.required_bytes()).unwrap();
            RdfPartitionStage::write_in(directory.path(), partition, reservation, &context)
                .unwrap()
                .authenticate(&parent, &limits, &context)
                .unwrap()
        };
        assert!(
            reader.descriptor().payload_bytes()
                <= u64::try_from(limits.max_total_extension_bytes).unwrap()
        );
        descriptors.push(reader.descriptor().clone());
        summaries.push(usage);
        drop(reader);
    }
    assert_eq!(
        descriptors
            .iter()
            .map(|descriptor| descriptor.payload_bytes())
            .sum::<u64>(),
        273_410_152
    );
    assert_eq!(
        descriptors
            .iter()
            .map(|descriptor| descriptor.record_count())
            .sum::<u64>(),
        520
    );
    assert_eq!(limits.max_total_extension_bytes, 268_435_456);
    assert!(
        descriptors
            .iter()
            .map(|descriptor| descriptor.payload_bytes())
            .sum::<u64>()
            > u64::try_from(limits.max_total_extension_bytes).unwrap()
    );
    assert!(
        summaries[0]
            .clone()
            .checked_add(summaries[1].clone(), &limits)
            .is_err()
    );
    eprintln!(
        "rdf partitions: files={}, records={}, payload_bytes={}, unchanged_owned_limit={}",
        descriptors.len(),
        descriptors
            .iter()
            .map(|descriptor| descriptor.record_count())
            .sum::<u64>(),
        descriptors
            .iter()
            .map(|descriptor| descriptor.payload_bytes())
            .sum::<u64>(),
        limits.max_total_extension_bytes
    );
    let mut document = parent.document().clone();
    document
        .extensions
        .extend(new_rdf_partition_set_envelopes(&parent, &descriptors, &context).unwrap());
    let total = u64::try_from(document.extensions.len()).unwrap() + 520;
    let per_file_totals: Vec<_> = descriptors
        .iter()
        .map(|descriptor| {
            let base_count = document
                .extensions
                .iter()
                .filter(|record| {
                    record
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == descriptor.file())
                })
                .count();
            (
                descriptor.file(),
                descriptor.record_count() + u64::try_from(base_count).unwrap(),
            )
        })
        .collect();
    let base = IdentityVerifiedRdfBase::verify(
        parent.metadata(),
        document,
        SourceFileCatalog::default(),
        &limits,
        &context,
    )
    .unwrap();
    drop(parent);
    OracleWriter::create_in(directory.path())
        .unwrap()
        .write_rdf_base(base, &context)
        .unwrap();
    let reader = OracleReader::open_in(directory.path(), &context).unwrap();
    assert_eq!(reader.stats().extensions(), total);
    assert_eq!(
        reader.stats().layout(),
        rootlight_storage::GenerationStatsLayout::CanonicalOracleWithRdfV1
    );
    assert_eq!(
        reader.read_generation(&context).unwrap_err().kind(),
        CatalogErrorKind::PartitionedMaterializationRequired
    );
    let mut cursor = None;
    let mut observed = std::collections::BTreeMap::<FileId, u64>::new();
    let mut last = None;
    loop {
        let page = reader
            .extensions(cursor, GenerationReadLimit::new(3).unwrap(), &context)
            .unwrap();
        assert_eq!(page.total_available(), total);
        for record in page.items() {
            last = Some(record.id);
            if record.namespace == rootlight_ir::RDF_STATEMENT_NAMESPACE {
                let statement = rootlight_ir::decode_rdf_statement_envelope(record).unwrap();
                statement.verify_source_text(&text).unwrap();
                let descriptor = descriptors
                    .iter()
                    .find(|descriptor| descriptor.file() == statement.source().span().file())
                    .unwrap();
                assert_eq!(statement.source().span().file(), descriptor.file());
                assert_eq!(statement.source().content_hash(), descriptor.source_hash());
                assert_eq!(
                    statement.object().literal_value().unwrap().len(),
                    256 * 1024
                );
                assert!(statement.source().line_hint().is_some());
                *observed.entry(descriptor.file()).or_default() += 1;
            }
        }
        if page.completeness() == ReadPageCompleteness::Complete {
            assert!(page.next_cursor().is_none());
            break;
        }
        cursor = page.next_cursor();
        assert!(cursor.is_some());
    }
    let exhausted = reader
        .extensions(last, GenerationReadLimit::new(3).unwrap(), &context)
        .unwrap();
    assert!(exhausted.items().is_empty());
    assert_eq!(exhausted.completeness(), ReadPageCompleteness::Complete);
    assert_eq!(exhausted.total_available(), total);
    for descriptor in descriptors {
        assert_eq!(
            observed.get(&descriptor.file()),
            Some(&descriptor.record_count())
        );
    }
    for (file, total) in per_file_totals {
        let page = reader
            .extensions_for_file(file, None, GenerationReadLimit::new(1).unwrap(), &context)
            .unwrap()
            .unwrap();
        assert_eq!(page.total_available(), total);
    }
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

fn indexed_fixture_snapshot() -> GenerationSnapshot {
    let mut document = fixture_documents().0;
    let provenance = &mut document.provenance[0];
    provenance.id =
        derive_provenance_record_id(provenance).expect("fixture provenance identity derives");
    let provenance_id = provenance.id;
    for record in &mut document.files {
        record.provenance = provenance_id;
    }
    for record in &mut document.entities {
        record.provenance = provenance_id;
    }
    for record in &mut document.occurrences {
        record.provenance = provenance_id;
        record.id =
            derive_occurrence_record_id(record).expect("fixture occurrence identity derives");
    }
    for record in &mut document.relations {
        record.provenance = provenance_id;
        record.id = derive_relation_record_id(record).expect("fixture relation identity derives");
    }
    for record in &mut document.source_mappings {
        record.provenance = provenance_id;
    }
    for record in &mut document.coverage_records {
        record.provenance = provenance_id;
        record.id = derive_coverage_record_id(record).expect("fixture coverage identity derives");
    }
    for record in &mut document.skipped_regions {
        record.provenance = provenance_id;
    }
    for record in &mut document.diagnostics {
        record.provenance = provenance_id;
    }
    for record in &mut document.extensions {
        record.provenance = provenance_id;
    }
    let mut second_extension = document.extensions[0].clone();
    second_extension.id = FactId::from_bytes([240; 20]);
    second_extension.payload = r#"{"marker":"second"}"#.to_owned();
    document.extensions.push(second_extension);

    let mut second_occurrence = document.occurrences[0].clone();
    second_occurrence.role = OccurrenceRole::Reference;
    second_occurrence.syntax_kind = "identifier.reference".to_owned();
    second_occurrence.id = derive_occurrence_record_id(&second_occurrence)
        .expect("second occurrence identity derives");
    document.occurrences.push(second_occurrence);

    let mut second_relation = document.relations[0].clone();
    second_relation.predicate = RelationPredicate::Contains;
    second_relation.id =
        derive_relation_record_id(&second_relation).expect("second relation identity derives");
    document.relations.push(second_relation);

    let mut second_coverage = document.coverage_records[0].clone();
    second_coverage.domain = FactDomain::Relations;
    second_coverage.discovered = 2;
    second_coverage.indexed = 2;
    second_coverage.skipped = 0;
    second_coverage.id =
        derive_coverage_record_id(&second_coverage).expect("second coverage identity derives");
    document.coverage_records.push(second_coverage);

    snapshot(document)
}

fn assert_indexed_conformance<R>(
    reader: &R,
    expected: &GenerationSnapshot,
    context: &GenerationContext<'_>,
) where
    R: GenerationReader<Error = CatalogError>,
{
    let document = expected.document();
    for file in &document.files {
        assert_eq!(
            reader.file(file.id, context).expect("file read succeeds"),
            Some(file.clone())
        );
    }
    assert_eq!(
        reader
            .file(FileId::from_bytes([0xff; 20]), context)
            .expect("missing file read succeeds"),
        None
    );
    let entity = &document.entities[0];
    assert_eq!(
        reader
            .entity(entity.id, context)
            .expect("entity read succeeds"),
        Some(entity.clone())
    );
    let provenance = &document.provenance[0];
    assert_eq!(
        reader
            .provenance(provenance.id, context)
            .expect("provenance read succeeds"),
        Some(provenance.clone())
    );

    let limit = GenerationReadLimit::new(1).expect("one-record page is valid");
    let relation_anchor = document.relations[0].subject;
    let relation_request =
        RelationReadRequest::new(relation_anchor, RelationReadDirection::Outgoing, limit);
    let first_relations = reader
        .relations(&relation_request, context)
        .expect("first relation page reads");
    assert_eq!(
        first_relations.total_available(),
        u64::try_from(document.relations.len()).expect("fixture relation count fits")
    );
    assert_eq!(
        first_relations.completeness(),
        ReadPageCompleteness::Truncated
    );
    let relation_cursor = first_relations
        .next_cursor()
        .expect("truncated relation page has a cursor");
    let second_relations = reader
        .relations(
            &relation_request.clone().with_after(relation_cursor),
            context,
        )
        .expect("second relation page reads");
    assert_eq!(
        second_relations.completeness(),
        ReadPageCompleteness::Complete
    );
    let mut observed_relations = first_relations.into_items();
    observed_relations.extend(second_relations.into_items());
    assert_eq!(observed_relations, document.relations);

    let predicate = document.relations[0].predicate;
    let filtered = reader
        .relations(
            &relation_request
                .with_predicates(vec![predicate])
                .expect("predicate filter is bounded"),
            context,
        )
        .expect("filtered relations read");
    assert_eq!(filtered.total_available(), 1);
    assert_eq!(filtered.items(), &[document.relations[0].clone()]);

    let occurrence_file = document.occurrences[0].file;
    let occurrence_request = OccurrenceReadRequest::new(occurrence_file, limit);
    let first_occurrences = reader
        .occurrences(&occurrence_request, context)
        .expect("first occurrence page reads");
    assert_eq!(
        first_occurrences.completeness(),
        ReadPageCompleteness::Truncated
    );
    let occurrence_cursor = first_occurrences
        .next_cursor()
        .expect("truncated occurrence page has a cursor");
    let second_occurrences = reader
        .occurrences(&occurrence_request.with_after(occurrence_cursor), context)
        .expect("second occurrence page reads");
    let mut observed_occurrences = first_occurrences.into_items();
    observed_occurrences.extend(second_occurrences.into_items());
    assert_eq!(observed_occurrences, document.occurrences);

    let coverage_scope = document.coverage_records[0].scope;
    let coverage_request = CoverageReadRequest::new(coverage_scope, limit);
    let first_coverage = reader
        .coverage(&coverage_request, context)
        .expect("first coverage page reads");
    assert_eq!(
        first_coverage.completeness(),
        ReadPageCompleteness::Truncated
    );
    let coverage_cursor = first_coverage
        .next_cursor()
        .expect("truncated coverage page has a cursor");
    let second_coverage = reader
        .coverage(&coverage_request.with_after(coverage_cursor), context)
        .expect("second coverage page reads");
    let mut observed_coverage = first_coverage.into_items();
    observed_coverage.extend(second_coverage.into_items());
    assert_eq!(observed_coverage, document.coverage_records);

    let first_extensions = reader
        .extensions(None, limit, context)
        .expect("first extension page reads");
    assert_eq!(first_extensions.total_available(), 2);
    assert_eq!(
        first_extensions.completeness(),
        ReadPageCompleteness::Truncated
    );
    assert_eq!(first_extensions.items(), &document.extensions[..1]);
    let extension_cursor = first_extensions
        .next_cursor()
        .expect("truncated extension page has a cursor");
    let second_extensions = reader
        .extensions(Some(extension_cursor), limit, context)
        .expect("second extension page reads");
    assert_eq!(second_extensions.total_available(), 2);
    assert_eq!(
        second_extensions.completeness(),
        ReadPageCompleteness::Complete
    );
    assert_eq!(second_extensions.next_cursor(), None);
    let mut observed_extensions = first_extensions.into_items();
    observed_extensions.extend(second_extensions.into_items());
    assert_eq!(observed_extensions, document.extensions);
    let exhausted = reader
        .extensions(Some(document.extensions[1].id), limit, context)
        .expect("exhausted extension cursor reads");
    assert!(exhausted.items().is_empty());
    assert_eq!(exhausted.total_available(), 2);
    assert_eq!(exhausted.completeness(), ReadPageCompleteness::Complete);
    assert_eq!(exhausted.next_cursor(), None);
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
fn indexed_readers_share_bounded_stable_cursor_semantics() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let expected = indexed_fixture_snapshot();
    let path_reader = OracleWriter::create_in(directory.path())
        .expect("path oracle target is created")
        .seal_unverified_for_test(expected.clone(), &context)
        .expect("path oracle seals");
    let ephemeral_reader = EphemeralOracleWriter::create()
        .expect("ephemeral oracle initializes")
        .seal_unverified_for_test(expected.clone(), &context)
        .expect("ephemeral oracle seals");

    assert_indexed_conformance(&path_reader, &expected, &context);
    assert_indexed_conformance(&ephemeral_reader, &expected, &context);

    let limit = GenerationReadLimit::new(1).unwrap();
    let defaults = GenerationBudget::default();
    let page_context = GenerationContext::new(
        &cancellation,
        GenerationBudget::new(
            8,
            defaults.limit(GenerationResource::SourceReferences),
            defaults.limit(GenerationResource::TextBytes),
        )
        .unwrap(),
    );
    assert!(path_reader.stats().stored_rows() > 8);
    for page in [
        path_reader.extensions(None, limit, &page_context).unwrap(),
        ephemeral_reader
            .extensions(None, limit, &page_context)
            .unwrap(),
    ] {
        assert_eq!(page.items(), &expected.document().extensions[..1]);
    }
    for error in [
        path_reader.read_generation(&page_context).unwrap_err(),
        ephemeral_reader.read_generation(&page_context).unwrap_err(),
    ] {
        assert!(matches!(
            error.kind(),
            CatalogErrorKind::BudgetExceeded {
                resource: GenerationResource::Rows,
                ..
            }
        ));
    }

    let file = expected.document().files[0].id;
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    let cancelled_context = default_context(&cancelled);
    for error in [
        path_reader
            .file(file, &cancelled_context)
            .expect_err("path indexed read observes cancellation"),
        ephemeral_reader
            .file(file, &cancelled_context)
            .expect_err("ephemeral indexed read observes cancellation"),
        path_reader
            .extensions(None, limit, &cancelled_context)
            .expect_err("path extension page observes cancellation"),
        ephemeral_reader
            .extensions(None, limit, &cancelled_context)
            .expect_err("ephemeral extension page observes cancellation"),
    ] {
        assert_eq!(error.kind(), CatalogErrorKind::Cancelled);
    }

    let constrained = Cancellation::new();
    let constrained_context = GenerationContext::new(
        &constrained,
        GenerationBudget::new(1, 1, 1).expect("tiny read budget is valid"),
    );
    for error in [
        path_reader
            .file(file, &constrained_context)
            .expect_err("path indexed read enforces its budget"),
        ephemeral_reader
            .file(file, &constrained_context)
            .expect_err("ephemeral indexed read enforces its budget"),
    ] {
        assert!(matches!(
            error.kind(),
            CatalogErrorKind::BudgetExceeded { .. }
        ));
    }
    for error in [
        path_reader
            .extensions(None, limit, &constrained_context)
            .unwrap_err(),
        ephemeral_reader
            .extensions(None, limit, &constrained_context)
            .unwrap_err(),
    ] {
        assert!(matches!(
            error.kind(),
            CatalogErrorKind::BudgetExceeded {
                resource: GenerationResource::Rows,
                ..
            }
        ));
    }
}

fn interleaved_extension_snapshot() -> GenerationSnapshot {
    let mut document = indexed_fixture_snapshot().document().clone();
    let template = document.extensions[0].clone();
    document.extensions.clear();
    for byte in 150_u8..156 {
        let file = &document.files[usize::from(byte % 2)];
        let mut record = template.clone();
        record.id = FactId::from_bytes([byte; 20]);
        record.payload =
            serde_json::json!({"other_file_text": document.files[0].id.to_string()}).to_string();
        record.evidence.source = Some(SourceRef::new(
            document.repository,
            document.generation,
            SourceSpan::new(file.id, 1, 2).unwrap(),
            file.content_hash,
            None,
        ));
        document.extensions.push(record);
    }
    let mut without_source = template;
    without_source.id = FactId::from_bytes([156; 20]);
    without_source.evidence.source = None;
    without_source.evidence.derivation = vec![rootlight_ir::FactRef::File(document.files[0].id)];
    document.extensions.push(without_source);
    snapshot(document)
}

#[test]
fn indexed_extension_file_pages_keep_interleaved_files_and_cursors_scoped() {
    let directory = TempDir::new().unwrap();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let expected = interleaved_extension_snapshot();
    let path_reader = OracleWriter::create_in(directory.path())
        .unwrap()
        .seal_unverified_for_test(expected.clone(), &context)
        .unwrap();
    let ephemeral_reader = EphemeralOracleWriter::create()
        .unwrap()
        .seal_unverified_for_test(expected.clone(), &context)
        .unwrap();
    let file = expected.document().files[0].id;
    let selected: Vec<_> = expected
        .document()
        .extensions
        .iter()
        .filter(|record| {
            record
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == file)
        })
        .cloned()
        .collect();
    assert_eq!(selected.len(), 3);
    let limit = GenerationReadLimit::new(1).unwrap();
    for reader in [
        &path_reader as &dyn GenerationReader<Error = CatalogError>,
        &ephemeral_reader,
    ] {
        let mut collected = Vec::new();
        let mut after = None;
        loop {
            let page = reader
                .extensions_for_file(file, after, limit, &context)
                .unwrap()
                .unwrap();
            assert_eq!(page.total_available(), 3);
            collected.extend_from_slice(page.items());
            after = page.next_cursor();
            if after.is_none() {
                break;
            }
        }
        assert_eq!(collected, selected);
        let after_other_file = reader
            .extensions_for_file(file, Some(FactId::from_bytes([151; 20])), limit, &context)
            .unwrap()
            .unwrap();
        assert_eq!(after_other_file.total_available(), 3);
        assert_eq!(after_other_file.items(), &selected[1..2]);
        let exhausted = reader
            .extensions_for_file(file, Some(FactId::from_bytes([255; 20])), limit, &context)
            .unwrap()
            .unwrap();
        assert_eq!(exhausted.total_available(), 3);
        assert!(exhausted.items().is_empty());
        assert_eq!(exhausted.completeness(), ReadPageCompleteness::Complete);
        let unknown = reader
            .extensions_for_file(FileId::from_bytes([254; 20]), None, limit, &context)
            .unwrap()
            .unwrap();
        assert_eq!(unknown.total_available(), 0);
        assert!(unknown.items().is_empty());
        assert_eq!(unknown.completeness(), ReadPageCompleteness::Complete);
    }
    let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
    for (sql, values) in [
        (
            crate::indexed_read::EXTENSIONS_FOR_FILE_COUNT_SQL,
            vec![rusqlite::types::Value::Text(file.to_string())],
        ),
        (
            crate::indexed_read::EXTENSIONS_FOR_FILE_FIRST_SQL,
            vec![
                rusqlite::types::Value::Text(file.to_string()),
                rusqlite::types::Value::Integer(2),
            ],
        ),
        (
            crate::indexed_read::EXTENSIONS_FOR_FILE_NEXT_SQL,
            vec![
                rusqlite::types::Value::Text(file.to_string()),
                rusqlite::types::Value::Blob(selected[0].id.as_bytes().to_vec()),
                rusqlite::types::Value::Integer(2),
            ],
        ),
    ] {
        let mut statement = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        let plan = statement
            .query_map(rusqlite::params_from_iter(values), |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|step| step.contains("SEARCH extensions USING")
                    && step.contains("extensions_by_source_file")),
            "{plan:?}"
        );
        assert!(
            !plan
                .iter()
                .any(|step| step.contains("SCAN extensions") || step.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }
}

#[test]
fn indexed_extension_file_pages_validate_row_owners_sources_and_payloads() {
    for field in [
        "id",
        "repository",
        "generation",
        "provenance",
        "row_repository",
        "row_generation",
        "row_provenance",
        "source_repository",
        "source_generation",
        "source_hash",
        "source_bounds",
        "unknown_source_file",
        "malformed_envelope",
    ] {
        let directory = TempDir::new().unwrap();
        let cancellation = Cancellation::new();
        let context = default_context(&cancellation);
        let expected = indexed_fixture_snapshot();
        let reader = OracleWriter::create_in(directory.path())
            .unwrap()
            .seal_unverified_for_test(expected.clone(), &context)
            .unwrap();
        let original = &expected.document().extensions[0];
        let source = original.evidence.source.as_ref().unwrap();
        let mut selected = source.span().file();
        let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .unwrap();
        if let Some(column) = field.strip_prefix("row_") {
            let sql = match column {
                "repository" => {
                    "UPDATE extensions SET repository_id = zeroblob(16) WHERE extension_id = ?1"
                }
                "generation" => {
                    "UPDATE extensions SET generation_id = zeroblob(20) WHERE extension_id = ?1"
                }
                "provenance" => {
                    "UPDATE extensions SET provenance_id = zeroblob(20) WHERE extension_id = ?1"
                }
                _ => unreachable!(),
            };
            connection
                .execute(sql, [original.id.as_bytes().as_slice()])
                .unwrap();
        } else {
            let mut value = serde_json::to_value(original).unwrap();
            let wrong_fact = serde_json::json!(FactId::from_bytes([201; 20]));
            match field {
                "id" | "provenance" => value[field] = wrong_fact,
                "repository" => {
                    value[field] =
                        serde_json::json!(rootlight_ids::RepositoryId::from_bytes([201; 16]))
                }
                "generation" => {
                    value[field] =
                        serde_json::json!(rootlight_ids::GenerationId::from_bytes([201; 20]))
                }
                "source_repository" => {
                    value["evidence"]["source"]["repository"] =
                        serde_json::json!(rootlight_ids::RepositoryId::from_bytes([201; 16]))
                }
                "source_generation" => {
                    value["evidence"]["source"]["generation"] =
                        serde_json::json!(rootlight_ids::GenerationId::from_bytes([201; 20]))
                }
                "source_hash" => {
                    value["evidence"]["source"]["content_hash"] =
                        serde_json::json!(content_hash(b"foreign source"))
                }
                "source_bounds" => {
                    value["evidence"]["source"]["span"]["end_byte"] = serde_json::json!(u64::MAX)
                }
                "unknown_source_file" => {
                    selected = FileId::from_bytes([254; 20]);
                    value["evidence"]["source"]["span"]["file"] = serde_json::json!(selected);
                }
                "malformed_envelope" => {
                    value.as_object_mut().unwrap().remove("namespace");
                }
                _ => unreachable!(),
            }
            connection
                .execute(
                    "UPDATE extensions SET payload = ?1 WHERE extension_id = ?2",
                    rusqlite::params![value.to_string(), original.id.as_bytes().as_slice()],
                )
                .unwrap();
        }
        drop(connection);
        assert_eq!(
            reader
                .extensions_for_file(
                    selected,
                    None,
                    GenerationReadLimit::new(1).unwrap(),
                    &context
                )
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Corrupt,
            "{field}"
        );
    }
}

#[test]
fn indexed_extension_file_pages_reject_cardinality_above_sealed_total() {
    let directory = TempDir::new().unwrap();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let expected = indexed_fixture_snapshot();
    let reader = OracleWriter::create_in(directory.path())
        .unwrap()
        .seal_unverified_for_test(expected.clone(), &context)
        .unwrap();
    let mut extra = expected.document().extensions[0].clone();
    extra.id = FactId::from_bytes([201; 20]);
    let file = extra.evidence.source.as_ref().unwrap().span().file();
    let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
    connection.execute(
        "INSERT INTO extensions(extension_id, repository_id, generation_id, provenance_id, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![extra.id.as_bytes().as_slice(), extra.repository.as_bytes().as_slice(), extra.generation.as_bytes().as_slice(), extra.provenance.as_bytes().as_slice(), serde_json::to_string(&extra).unwrap()],
    ).unwrap();
    drop(connection);
    for after in [None, Some(FactId::from_bytes([255; 20]))] {
        assert_eq!(
            reader
                .extensions_for_file(file, after, GenerationReadLimit::new(1).unwrap(), &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Corrupt
        );
    }
}

#[test]
fn indexed_extension_file_pages_charge_budgets_and_observe_cancellation() {
    let directory = TempDir::new().unwrap();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let expected = indexed_fixture_snapshot();
    let file = expected.document().extensions[0]
        .evidence
        .source
        .as_ref()
        .unwrap()
        .span()
        .file();
    let path_reader = OracleWriter::create_in(directory.path())
        .unwrap()
        .seal_unverified_for_test(expected.clone(), &context)
        .unwrap();
    let ephemeral_reader = EphemeralOracleWriter::create()
        .unwrap()
        .seal_unverified_for_test(expected, &context)
        .unwrap();
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    let cancelled_context = default_context(&cancelled);
    let limit = GenerationReadLimit::new(1).unwrap();
    for reader in [
        &path_reader as &dyn GenerationReader<Error = CatalogError>,
        &ephemeral_reader,
    ] {
        assert_eq!(
            reader
                .extensions_for_file(file, None, limit, &cancelled_context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Cancelled
        );
        for (budget, resource) in [
            (
                GenerationBudget::new(1, 100, 1000).unwrap(),
                GenerationResource::Rows,
            ),
            (
                GenerationBudget::new(100, 1, 1000).unwrap(),
                GenerationResource::SourceReferences,
            ),
            (
                GenerationBudget::new(100, 100, 1).unwrap(),
                GenerationResource::TextBytes,
            ),
        ] {
            let constrained = GenerationContext::new(&cancellation, budget);
            assert!(
                matches!(reader.extensions_for_file(file, None, limit, &constrained).unwrap_err().kind(), CatalogErrorKind::BudgetExceeded { resource: actual, .. } if actual == resource)
            );
        }
    }
}

#[test]
fn indexed_extension_file_pages_charge_encoded_bytes_before_malformed_decode() {
    let directory = TempDir::new().unwrap();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let expected = indexed_fixture_snapshot();
    let reader = OracleWriter::create_in(directory.path())
        .unwrap()
        .seal_unverified_for_test(expected.clone(), &context)
        .unwrap();
    let original = &expected.document().extensions[0];
    let file = original.evidence.source.as_ref().unwrap().span().file();
    let mut value = serde_json::to_value(original).unwrap();
    value.as_object_mut().unwrap().remove("namespace");
    value["payload"] = serde_json::json!("x".repeat(100_000));
    let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
    connection
        .execute(
            "UPDATE extensions SET payload = ?1 WHERE extension_id = ?2",
            rusqlite::params![value.to_string(), original.id.as_bytes().as_slice()],
        )
        .unwrap();
    drop(connection);
    let constrained =
        GenerationContext::new(&cancellation, GenerationBudget::new(100, 100, 512).unwrap());
    assert!(matches!(
        reader
            .extensions_for_file(
                file,
                None,
                GenerationReadLimit::new(1).unwrap(),
                &constrained
            )
            .unwrap_err()
            .kind(),
        CatalogErrorKind::BudgetExceeded {
            resource: GenerationResource::EncodedTextBytes,
            ..
        }
    ));
    assert_eq!(
        reader
            .extensions_for_file(file, None, GenerationReadLimit::new(1).unwrap(), &context)
            .unwrap_err()
            .kind(),
        CatalogErrorKind::Corrupt
    );
}

#[test]
fn indexed_extension_file_index_rejects_malformed_json_writes_atomically() {
    let directory = TempDir::new().unwrap();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let expected = indexed_fixture_snapshot();
    let reader = OracleWriter::create_in(directory.path())
        .unwrap()
        .seal_unverified_for_test(expected.clone(), &context)
        .unwrap();
    let record = &expected.document().extensions[0];
    let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
    let error = connection
        .execute(
            "UPDATE extensions SET payload = ?1 WHERE extension_id = ?2",
            rusqlite::params!["{".repeat(4096), record.id.as_bytes().as_slice()],
        )
        .unwrap_err();
    assert!(error.to_string().contains("malformed JSON"), "{error}");
    drop(connection);
    let page = reader
        .extensions_for_file(
            record.evidence.source.as_ref().unwrap().span().file(),
            None,
            GenerationReadLimit::new(1).unwrap(),
            &context,
        )
        .unwrap()
        .unwrap();
    assert_eq!(page.items(), &expected.document().extensions[..1]);
    assert_eq!(reader.read(&context).unwrap(), expected);
}

#[test]
fn indexed_extension_file_pages_round_trip_on_disk_and_in_memory() {
    let directory = TempDir::new().unwrap();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let expected = indexed_fixture_snapshot();
    let source_file = expected.document().extensions[0]
        .evidence
        .source
        .as_ref()
        .unwrap()
        .span()
        .file();
    let path_reader = OracleWriter::create_in(directory.path())
        .unwrap()
        .seal_unverified_for_test(expected.clone(), &context)
        .unwrap();
    let ephemeral_reader = EphemeralOracleWriter::create()
        .unwrap()
        .seal_unverified_for_test(expected.clone(), &context)
        .unwrap();
    let limit = GenerationReadLimit::new(1).unwrap();
    for reader in [
        &path_reader as &dyn GenerationReader<Error = CatalogError>,
        &ephemeral_reader,
    ] {
        let first = reader
            .extensions_for_file(source_file, None, limit, &context)
            .unwrap()
            .unwrap();
        assert_eq!(first.total_available(), 2);
        assert_eq!(first.items(), &expected.document().extensions[..1]);
        assert_eq!(first.completeness(), ReadPageCompleteness::Truncated);
        let second = reader
            .extensions_for_file(source_file, first.next_cursor(), limit, &context)
            .unwrap()
            .unwrap();
        assert_eq!(second.total_available(), 2);
        assert_eq!(second.items(), &expected.document().extensions[1..]);
        assert_eq!(second.completeness(), ReadPageCompleteness::Complete);
    }
}

#[test]
fn extension_pages_enforce_encoded_text_before_decoding_malformed_payloads() {
    let directory = TempDir::new().unwrap();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let expected = indexed_fixture_snapshot();
    let reader = OracleWriter::create_in(directory.path())
        .unwrap()
        .seal_unverified_for_test(expected.clone(), &context)
        .unwrap();
    let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
    connection
        .execute(
            "UPDATE extensions SET payload = ?1 WHERE extension_id = ?2",
            rusqlite::params![
                format!("\"{}\"", "{".repeat(4094)),
                expected.document().extensions[0].id.as_bytes().as_slice()
            ],
        )
        .unwrap();
    drop(connection);
    let tiny_text =
        GenerationContext::new(&cancellation, GenerationBudget::new(100, 100, 1).unwrap());
    let limit = GenerationReadLimit::new(1).unwrap();
    let expected_limit = tiny_text
        .budget()
        .limit(GenerationResource::EncodedTextBytes);
    let error = reader.extensions(None, limit, &tiny_text).unwrap_err();
    assert!(
        matches!(error.kind(), CatalogErrorKind::BudgetExceeded {
            resource: GenerationResource::EncodedTextBytes,
            observed: 4096,
            limit,
        } if limit == expected_limit),
        "{error:?}"
    );
    assert_eq!(
        reader.extensions(None, limit, &context).unwrap_err().kind(),
        CatalogErrorKind::Corrupt
    );
}

#[test]
fn extension_pages_reject_payload_and_row_identity_mismatches() {
    for field in [
        "id",
        "repository",
        "generation",
        "provenance",
        "row_repository",
        "row_generation",
        "row_provenance",
    ] {
        let directory = TempDir::new().unwrap();
        let cancellation = Cancellation::new();
        let context = default_context(&cancellation);
        let expected = indexed_fixture_snapshot();
        let reader = OracleWriter::create_in(directory.path())
            .unwrap()
            .seal_unverified_for_test(expected.clone(), &context)
            .unwrap();
        let original = &expected.document().extensions[0];
        let connection = Connection::open(directory.path().join(ORACLE_FILENAME)).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .unwrap();
        if let Some(column) = field.strip_prefix("row_") {
            let sql = match column {
                "repository" => {
                    "UPDATE extensions SET repository_id = zeroblob(16) WHERE extension_id = ?1"
                }
                "generation" => {
                    "UPDATE extensions SET generation_id = zeroblob(20) WHERE extension_id = ?1"
                }
                "provenance" => {
                    "UPDATE extensions SET provenance_id = zeroblob(20) WHERE extension_id = ?1"
                }
                _ => unreachable!(),
            };
            connection
                .execute(sql, [original.id.as_bytes().as_slice()])
                .unwrap();
        } else {
            let mut changed = original.clone();
            match field {
                "id" => changed.id = FactId::from_bytes([201; 20]),
                "repository" => {
                    changed.repository = rootlight_ids::RepositoryId::from_bytes([201; 16])
                }
                "generation" => {
                    changed.generation = rootlight_ids::GenerationId::from_bytes([201; 20])
                }
                "provenance" => changed.provenance = FactId::from_bytes([201; 20]),
                _ => unreachable!(),
            }
            connection
                .execute(
                    "UPDATE extensions SET payload = ?1 WHERE extension_id = ?2",
                    rusqlite::params![
                        serde_json::to_string(&changed).unwrap(),
                        original.id.as_bytes().as_slice()
                    ],
                )
                .unwrap();
        }
        drop(connection);
        assert_eq!(
            reader
                .extensions(None, GenerationReadLimit::new(1).unwrap(), &context)
                .unwrap_err()
                .kind(),
            CatalogErrorKind::Corrupt,
            "{field}"
        );
    }
}

#[test]
fn signature_batch_seal_reopen_recovers_exact_child_headers_and_subject_postings() {
    let directory = TempDir::new().unwrap();
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let mut document = indexed_fixture_snapshot().document().clone();
    let first = document.entities[0].clone();
    let mut second = first.clone();
    second.id = rootlight_ids::SymbolId::from_bytes([202; 20]);
    second.canonical_name = "second_header".to_owned();
    second.display_name = "second_header".to_owned();
    second.qualified_name = "second_header".to_owned();
    let file = document
        .files
        .iter()
        .find(|file| file.id == first.evidence.source.as_ref().unwrap().span().file())
        .unwrap();
    second.evidence.source = Some(SourceRef::new(
        document.repository,
        document.generation,
        SourceSpan::new(file.id, 1, 2).unwrap(),
        file.content_hash,
        None,
    ));
    document.entities.push(second.clone());
    let children = [
        (&first, "fn first_header()"),
        (&second, "(defun second_header ())"),
    ]
    .into_iter()
    .map(|(entity, text)| {
        let lexical = rootlight_ir::LexicalEvidenceV1::from_complete_text(
            rootlight_ir::LexicalEvidenceKind::Signature,
            rootlight_ir::FactRef::Entity(entity.id),
            rootlight_ir::LexicalEvidenceFormat::SourceText,
            text,
        )
        .unwrap();
        rootlight_ir::new_lexical_evidence_envelope(
            document.repository,
            document.generation,
            entity.provenance,
            entity.evidence.source.clone().unwrap(),
            &lexical,
        )
        .unwrap()
    })
    .collect::<Vec<_>>();
    document
        .extensions
        .push(rootlight_ir::new_lexical_signature_batch_envelope(&children).unwrap());
    let expected = snapshot(document);
    let reader = OracleWriter::create_in(directory.path())
        .unwrap()
        .seal_unverified_for_test(expected.clone(), &context)
        .unwrap();
    drop(reader);
    let reader = OracleReader::open_in(directory.path(), &context).unwrap();
    let reopened = reader.read(&context).unwrap();
    assert_eq!(reopened, expected);
    let ordinal = reopened
        .document()
        .extensions
        .iter()
        .position(|extension| extension.version == rootlight_ir::LEXICAL_SIGNATURE_BATCH_VERSION)
        .unwrap();
    assert_eq!(reopened.lexical_ordinals_for_symbol(first.id), &[ordinal]);
    assert_eq!(reopened.lexical_ordinals_for_symbol(second.id), &[ordinal]);
    assert!(
        reopened
            .lexical_ordinals_for_symbol(rootlight_ids::SymbolId::from_bytes([203; 20]))
            .is_empty()
    );
    let reconstructed = rootlight_ir::decode_lexical_signature_batch_envelope(
        &reopened.document().extensions[ordinal],
    )
    .unwrap();
    let mut expected_children = children;
    expected_children.sort_unstable_by_key(|child| child.id);
    assert_eq!(reconstructed, expected_children);
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
fn bulk_load_keeps_deferred_cycle_lookup_index() {
    let directory = TempDir::new().expect("temporary generation directory is created");
    let OracleWriter {
        mut connection,
        path: _,
    } = OracleWriter::create_in(directory.path()).expect("oracle schema initializes");
    let expected_index_count = usize::try_from(
        connection
            .query_row(
                "SELECT count(*)
             FROM sqlite_schema
             WHERE type = 'index' AND name NOT LIKE 'sqlite_autoindex_%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("secondary index count is readable"),
    )
    .expect("secondary index count fits memory");
    let expected_deferred_index_count = expected_index_count
        .checked_sub(1)
        .expect("the deferred-cycle lookup index exists");
    assert!(expected_deferred_index_count > 0);

    let dropped_indexes = Arc::new(AtomicUsize::new(0));
    let payload_started = Arc::new(AtomicBool::new(false));
    let rebuilt_indexes = Arc::new(AtomicUsize::new(0));
    let observed_dropped = Arc::clone(&dropped_indexes);
    let observed_payload = Arc::clone(&payload_started);
    let observed_rebuilt = Arc::clone(&rebuilt_indexes);
    connection
        .authorizer(Some(
            move |context: rusqlite::hooks::AuthContext<'_>| match context.action {
                AuthAction::DropIndex { index_name, .. } => {
                    if index_name == "source_refs_by_file_span" {
                        Authorization::Deny
                    } else {
                        observed_dropped.fetch_add(1, Ordering::Relaxed);
                        Authorization::Allow
                    }
                }
                AuthAction::Insert {
                    table_name: "generation_meta",
                } => {
                    if observed_dropped.load(Ordering::Relaxed) == expected_deferred_index_count {
                        observed_payload.store(true, Ordering::Relaxed);
                        Authorization::Allow
                    } else {
                        Authorization::Deny
                    }
                }
                AuthAction::CreateIndex { index_name, .. } => {
                    if index_name == "source_refs_by_file_span" {
                        Authorization::Deny
                    } else if observed_payload.load(Ordering::Relaxed) {
                        observed_rebuilt.fetch_add(1, Ordering::Relaxed);
                        Authorization::Allow
                    } else {
                        Authorization::Deny
                    }
                }
                _ => Authorization::Allow,
            },
        ))
        .expect("test authorizer installs");

    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    crate::schema::install_generation_cancellation(&connection, &context)
        .expect("generation cancellation installs");
    let generation = snapshot(fixture_documents().0);
    crate::write::write_generation(&mut connection, &generation, &context)
        .expect("generation seals with deferred secondary indexes");

    assert!(payload_started.load(Ordering::Relaxed));
    assert_eq!(
        dropped_indexes.load(Ordering::Relaxed),
        expected_deferred_index_count
    );
    assert_eq!(
        rebuilt_indexes.load(Ordering::Relaxed),
        expected_deferred_index_count
    );
    crate::schema::validate_oracle(&connection, &context)
        .expect("rebuilt oracle retains the exact sealed schema");
}

#[cfg(windows)]
#[test]
fn windows_oracle_round_trip_crosses_legacy_journal_path_limit() {
    use std::os::windows::ffi::OsStrExt as _;

    fn utf16_len(path: &Path) -> usize {
        path.as_os_str().encode_wide().count()
    }

    let temporary = TempDir::new().expect("temporary generation root is created");
    let root = fs::canonicalize(temporary.path()).expect("temporary root canonicalizes");
    let unpadded_database_length = utf16_len(&root.join(ORACLE_FILENAME));
    let padding_length = 254_usize
        .checked_sub(unpadded_database_length + 1)
        .expect("temporary root leaves room for the long-path fixture");
    assert!(
        padding_length <= 255,
        "fixture padding must fit one Windows path component"
    );
    let generation_directory = root.join("x".repeat(padding_length));
    fs::create_dir(&generation_directory).expect("long generation directory is created");

    let database_path = generation_directory.join(ORACLE_FILENAME);
    let mut journal_name = database_path.as_os_str().to_os_string();
    journal_name.push("-journal");
    let journal_path = PathBuf::from(journal_name);
    assert_eq!(utf16_len(&database_path), 254);
    assert!(utf16_len(&journal_path) > 260);

    let expected = write_fixture(&generation_directory);
    let cancellation = Cancellation::new();
    let context = default_context(&cancellation);
    let reopened = OracleReader::open_in(&generation_directory, &context)
        .expect("sealed oracle reopens across the legacy path limit");
    assert_eq!(
        reopened
            .read(&context)
            .expect("reopened long-path oracle remains readable"),
        expected
    );
    assert!(database_path.is_file());
    assert!(!journal_path.exists());
}

#[test]
fn preflight_budget_and_cancellation_leave_no_payload_rows() {
    let budget_directory = TempDir::new().expect("budget staging directory is created");
    let cancellation = Cancellation::new();
    let budget =
        GenerationBudget::new(1_000, 1_000, 1).expect("encoded-text-constrained budget is valid");
    let context = GenerationContext::new(&cancellation, budget);
    let error = OracleWriter::create_in(budget_directory.path())
        .expect("oracle schema initializes")
        .seal_unverified_for_test(snapshot(fixture_documents().0), &context)
        .expect_err("generation exceeds the tiny budget");
    let CatalogErrorKind::BudgetExceeded {
        resource,
        observed,
        limit,
    } = error.kind()
    else {
        panic!("tiny generation must return a typed budget error");
    };
    assert_eq!(resource, GenerationResource::EncodedTextBytes);
    assert!(observed > limit);
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
    assert_eq!(oracle.schema_version(), 9);
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
    let root_path = canonical_test_root(&directory);
    fs::create_dir(root_path.join("src")).expect("source directory is created");
    fs::write(root_path.join("src/lib.rs"), SOURCE).expect("source fixture is written");
    let generation_directory = root_path.join("generation");
    fs::create_dir(&generation_directory).expect("generation directory is created");

    let repository = derive_repository(b"catalog-real-treesitter").id();
    let relative = RelativePath::parse(Path::new("src/lib.rs")).expect("relative path is valid");
    let root = RepositoryRoot::open(repository, &root_path).expect("repository root opens");
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
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
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
    assert_eq!(
        error,
        IdentityVerificationError::IdentityMismatch(
            rootlight_storage::IdentityMismatchComponent::Coverage,
        )
    );

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
    assert_eq!(
        error,
        IdentityVerificationError::IdentityMismatch(
            rootlight_storage::IdentityMismatchComponent::FileClaim,
        )
    );

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
    assert_eq!(
        error,
        IdentityVerificationError::IdentityMismatch(
            rootlight_storage::IdentityMismatchComponent::SymbolClaim,
        )
    );

    let prospective =
        crate::OracleLayoutFragment::measure(verified.document(), 0, &context, &mut |_| {
            Ok::<_, ()>(())
        })
        .unwrap()
        .read_memory_plan(&context)
        .unwrap();
    assert!(prospective.retained_text_bytes() < prospective.stats().text_bytes());
    let (reader, verified, producer_plan) = OracleWriter::create_in(&generation_directory)
        .expect("oracle target is created")
        .seal_preserving_verified_with_read_plan(verified, &context)
        .expect("verified Tree-sitter generation seals");
    assert_eq!(producer_plan, prospective);
    assert_eq!(verified.document(), output.document());
    let mut independent_plan = None;
    let refused = reader.read_with_admission(&context, |plan| {
        independent_plan = Some(plan);
        Err::<(), _>("before materialization")
    });
    assert!(matches!(
        refused,
        Err(crate::OracleReadAdmissionError::Admission(
            "before materialization"
        ))
    ));
    assert_eq!(independent_plan, Some(producer_plan));
    let queried = reader
        .read_generation(&context)
        .expect("backend-neutral verified query succeeds");
    assert_eq!(queried.document(), output.document());
}

#[test]
fn independent_sdk_producer_uses_the_same_identity_verifier() {
    const SOURCE: &str = "opaque mock input\n";
    let directory = TempDir::new().expect("temporary source root is created");
    let root_path = canonical_test_root(&directory);
    fs::write(root_path.join("input.txt"), SOURCE).expect("source fixture is written");
    let repository = derive_repository(b"catalog-independent-sdk").id();
    let relative = RelativePath::parse(Path::new("input.txt")).expect("relative path is valid");
    let root = RepositoryRoot::open(repository, &root_path).expect("repository root opens");
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
