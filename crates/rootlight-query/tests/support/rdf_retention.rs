//! Logical RDF retention over a genuinely authenticated complete catalog.

use super::*;
use rootlight_catalog::{OracleReader, OracleWriter, RdfPartitionStage};
use rootlight_ir::{
    FileIdentityClaim, RdfStatement, derive_provenance_record_id, new_file_identity_claim_envelope,
    new_rdf_statement_envelope,
};
use rootlight_storage::{
    GenerationReadLimit, GenerationReader, IdentityVerifiedRdfBase, SourceFileCatalog,
    UnpublishedRdfPartition, new_rdf_partition_set_envelopes,
};

fn write_logical_rdf_fixture(
    directory: &Path,
    context: &GenerationContext<'_>,
) -> (GenerationMetadata, FactId) {
    let text = "_:a <urn:p> \"value\" .";
    let repository = derive_repository(b"query-rdf-retention").id();
    let configuration_hash = content_hash(b"query-rdf-retention-configuration");
    let provider_set_hash = content_hash(b"query-rdf-retention-provider");
    let path = RelativePath::parse(Path::new("data.nt")).unwrap();
    let file = derive_file(FileIdentity {
        repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    let byte_length = u64::try_from(text.len()).unwrap();
    let claim = FileIdentityClaim {
        file,
        repository,
        path: path.as_str().into(),
        path_identity: path.identity_bytes().to_vec(),
        content_hash: content_hash(text.as_bytes()),
        byte_length,
    };
    let manifest_hash =
        GenerationManifestRecipe::new(repository, configuration_hash, vec![claim.clone()])
            .unwrap()
            .canonical_hash()
            .unwrap();
    let generation = derive_generation(GenerationIdentity {
        repository,
        parent: None,
        manifest_hash,
        config_hash: configuration_hash,
        provider_set_hash,
        format_version: generation_format_version(),
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
    let source = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(file, 0, byte_length).unwrap(),
        claim.content_hash,
        None,
    );
    let mut provenance = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        producer_kind: ProducerKind::Parser,
        producer: ProducerIdentity::new("query-rdf-retention", "1", configuration_hash).unwrap(),
        binary_digest: content_hash(b"query-rdf-retention-test"),
        frontend_version: Some("ntriples-1".into()),
        language: "ntriples".into(),
        tier: AnalysisTier::TierC,
        build_context: BuildContextIdentity::new(configuration_hash),
        input_sources: vec![source.clone()],
        evidence_sources: vec![source.clone()],
        derivation_parents: Vec::new(),
        rule: None,
    };
    provenance.id = derive_provenance_record_id(&provenance).unwrap();
    let mut document = NormalizedIrDocument::empty(repository, generation);
    document.files.push(FileRecord {
        id: file,
        repository,
        generation,
        path: claim.path.clone(),
        path_locator: Some(path.to_locator()),
        content_hash: claim.content_hash,
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
        new_file_identity_claim_envelope(&claim, generation, provenance.id, source.clone())
            .unwrap(),
    );
    document.provenance.push(provenance);
    let limits = IrLimits::default();
    let parent = IdentityVerifiedGeneration::verify(
        metadata,
        document,
        &limits,
        &ExtensionSupport::default(),
        context,
    )
    .unwrap();
    let statement = RdfStatement::from_ntriples(
        source,
        text,
        [
            SourceSpan::new(file, 0, 3).unwrap(),
            SourceSpan::new(file, 4, 11).unwrap(),
            SourceSpan::new(file, 12, 19).unwrap(),
        ],
    )
    .unwrap();
    let record =
        new_rdf_statement_envelope(parent.document().provenance[0].id, &statement).unwrap();
    let statement_id = record.id;
    let partition = UnpublishedRdfPartition::admit(
        metadata,
        file,
        claim.content_hash,
        byte_length,
        vec![record],
        &limits,
        context,
    )
    .unwrap();
    let estimate = RdfPartitionStage::storage_estimate(&partition).unwrap();
    let partition = RdfPartitionStage::write_in(
        directory,
        partition,
        estimate.admit(estimate.required_bytes()).unwrap(),
        context,
    )
    .unwrap()
    .authenticate(&parent, &limits, context)
    .unwrap();
    let mut base = parent.document().clone();
    base.extensions.extend(
        new_rdf_partition_set_envelopes(&parent, &[partition.descriptor().clone()], context)
            .unwrap(),
    );
    let base = IdentityVerifiedRdfBase::verify(
        metadata,
        base,
        SourceFileCatalog::default(),
        &limits,
        context,
    )
    .unwrap();
    OracleWriter::create_in(directory)
        .unwrap()
        .write_rdf_base(base, context)
        .unwrap();
    (metadata, statement_id)
}

fn search(generation: rootlight_ids::GenerationId) -> FakeSearch {
    FakeSearch {
        generation,
        hits: Vec::new(),
    }
}

#[test]
fn logical_rdf_retention_pins_reader_and_reloads_without_owned_snapshot_authority() {
    let directory = tempdir_in(std::env::temp_dir()).unwrap();
    let canonical_directory = tempdir_in(std::env::temp_dir()).unwrap();
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let (metadata, statement) = write_logical_rdf_fixture(directory.path(), &context);
    let generation = metadata.generation();
    let reopen = || OracleReader::open_in(directory.path(), &context).unwrap();
    let foreign = verified_empty_generation(211).metadata().generation();
    let canonical = OracleWriter::create_in(canonical_directory.path())
        .unwrap()
        .seal(verified_empty_generation(211), &context)
        .unwrap();
    let mut generations = GenerationSet::new(1).unwrap();
    assert!(matches!(
        generations.stage_rdf(canonical, search(foreign)),
        Err(QueryError::GenerationMismatch)
    ));
    assert!(matches!(
        generations.stage_rdf(reopen(), search(foreign)),
        Err(QueryError::GenerationMismatch)
    ));
    assert!(generations.is_empty());
    assert_eq!(generations.active_generation(), None);
    generations.stage_rdf(reopen(), search(generation)).unwrap();
    assert!(!generations.contains(generation));
    assert!(matches!(
        generations.rdf_lease(generation),
        Err(QueryError::GenerationNotFound)
    ));
    assert!(matches!(
        generations.stage_rdf(reopen(), search(generation)),
        Err(QueryError::DuplicateGeneration)
    ));
    assert!(matches!(
        generations.stage(verified_empty_generation(211), search(foreign)),
        Err(QueryError::RetentionLimit)
    ));
    generations.commit_staged(generation, false).unwrap();
    assert_eq!(generations.active_generation(), None);
    generations.activate(generation).unwrap();
    assert_eq!(generations.active_generation(), Some(generation));
    assert_eq!(generations.metadata(generation).unwrap(), metadata);
    assert!(matches!(
        generations.query(generation),
        Err(QueryError::LogicalGenerationRequiresReader)
    ));
    assert!(matches!(
        generations.lease(generation),
        Err(QueryError::LogicalGenerationRequiresReader)
    ));
    assert!(matches!(
        generations.generation(generation),
        Err(QueryError::LogicalGenerationRequiresReader)
    ));
    let lease = generations.rdf_lease(generation).unwrap();
    assert_eq!(lease.reader().metadata(), metadata);
    assert_eq!(lease.search().generation(), generation);
    assert!(!generations.unload(generation).unwrap());
    assert!(matches!(
        generations.reload_rdf(reopen(), search(generation)),
        Err(QueryError::DuplicateGeneration)
    ));
    assert_eq!(generations.metadata(generation).unwrap(), metadata);
    drop(lease);
    assert!(generations.unload(generation).unwrap());
    assert!(!generations.is_loaded(generation));
    assert_eq!(generations.metadata(generation).unwrap(), metadata);
    assert!(matches!(
        generations.rdf_lease(generation),
        Err(QueryError::GenerationNotFound)
    ));
    assert!(matches!(
        generations.reload_rdf(reopen(), search(foreign)),
        Err(QueryError::GenerationMismatch)
    ));
    assert!(!generations.is_loaded(generation));
    assert_eq!(generations.active_generation(), Some(generation));
    generations
        .reload_rdf(reopen(), search(generation))
        .unwrap();
    let lease = generations.rdf_lease(generation).unwrap();
    assert_eq!(lease.reader().metadata(), metadata);
    generations.rollback_commit(generation, None).unwrap();
    generations.discard_staged(generation).unwrap();
    drop(generations);
    let page = lease
        .reader()
        .extensions(None, GenerationReadLimit::default(), &context)
        .unwrap();
    assert!(page.items().iter().any(|record| record.id == statement));
    assert_eq!(lease.search().generation(), generation);
}
