//! Differential and adversarial coverage for the owned-byte segment prototype.
//!
//! A hand-built identity-verified graph is read through both normalized SQLite
//! and the segment reader, then format corruption and cancellation are probed.

use std::sync::Arc;

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_catalog::{EphemeralOracleReader, EphemeralOracleWriter};
use rootlight_ids::{
    FactId, FileIdentity, GenerationIdentity, content_hash, derive_file, derive_generation,
    derive_repository,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, Confidence, ContainerRef, CoverageRecord, CoverageScope,
    CoverageStatus, EntityKind, EntityRecord, EntityVisibility, EvidenceKind, ExtensionSupport,
    FactDomain, FactEvidence, FileIdentityClaim, FileRecord, IrLimits, NormalizedIrDocument,
    OccurrenceRecord, OccurrenceRole, OccurrenceTarget, ProducerIdentity, ProducerKind,
    ProvenanceRecord, RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
    SymbolIdentityClaim, derive_coverage_record_id, derive_occurrence_record_id,
    derive_provenance_record_id, derive_relation_record_id, new_file_identity_claim_envelope,
    new_symbol_identity_claim_envelope,
};
use rootlight_segment::{Segment, SegmentError, SegmentReader};
use rootlight_storage::{
    CoverageReadRequest, GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext,
    GenerationManifestRecipe, GenerationMetadata, GenerationReadLimit, GenerationReader,
    IdentityVerifiedGeneration, OccurrenceReadRequest, RelationReadDirection, RelationReadRequest,
};

const HEADER_BYTES: usize = 576;
const HEADER_CHECKSUM_START: usize = 32;
const HEADER_CHECKSUM_END: usize = 64;
const DESCRIPTOR_START: usize = 64;
const DESCRIPTOR_BYTES: usize = 64;

fn context(cancellation: &Cancellation) -> GenerationContext<'_> {
    GenerationContext::new(cancellation, GenerationBudget::default())
}

fn fixture() -> (GenerationMetadata, NormalizedIrDocument) {
    const SOURCE: &[u8] = b"pub fn answer() -> u32 { answer() }\n";
    const PATH: &str = "src/lib.rs";
    let repository = derive_repository(b"rootlight-segment-differential").id();
    let path_identity = PATH.as_bytes().to_vec();
    let file = derive_file(FileIdentity {
        repository,
        path_identity: &path_identity,
    })
    .id();
    let source_hash = content_hash(SOURCE);
    let configuration_hash = content_hash(b"segment-fixture-configuration");
    let mut file_claim = FileIdentityClaim {
        file,
        repository,
        path: PATH.to_owned(),
        path_identity,
        content_hash: source_hash,
        byte_length: u64::try_from(SOURCE.len()).expect("fixture length fits u64"),
    };
    file_claim.file = file_claim.derived_file();
    let manifest_hash =
        GenerationManifestRecipe::new(repository, configuration_hash, vec![file_claim.clone()])
            .expect("fixture manifest is valid")
            .canonical_hash()
            .expect("fixture manifest encodes");
    let provider_set_hash = content_hash(b"segment-fixture-providers");
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
    let full_source = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(
            file,
            0,
            u64::try_from(SOURCE.len()).expect("fixture length fits u64"),
        )
        .expect("fixture span is valid"),
        source_hash,
        None,
    );
    let definition_source = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(file, 7, 13).expect("definition span is valid"),
        source_hash,
        None,
    );
    let call_source = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(file, 25, 33).expect("call span is valid"),
        source_hash,
        None,
    );
    let build_context = BuildContextIdentity::new(content_hash(b"segment-fixture-build"));
    let mut provenance = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        producer_kind: ProducerKind::Parser,
        producer: ProducerIdentity::new(
            "rootlight-segment-fixture",
            "1.0",
            content_hash(b"segment-fixture-producer"),
        )
        .expect("fixture producer is valid"),
        binary_digest: content_hash(b"segment-fixture-binary"),
        frontend_version: Some("fixture-1".to_owned()),
        language: "rust".to_owned(),
        tier: AnalysisTier::TierB,
        build_context,
        input_sources: vec![full_source.clone()],
        evidence_sources: vec![definition_source.clone(), call_source.clone()],
        derivation_parents: Vec::new(),
        rule: None,
    };
    provenance.id = derive_provenance_record_id(&provenance).expect("provenance identity derives");

    let file_record = FileRecord {
        id: file,
        repository,
        generation,
        path: PATH.to_owned(),
        path_locator: None,
        content_hash: source_hash,
        byte_length: u64::try_from(SOURCE.len()).expect("fixture length fits u64"),
        language: "rust".to_owned(),
        encoding: "utf-8".to_owned(),
        generated: false,
        provenance: provenance.id,
        evidence: FactEvidence {
            source: Some(full_source.clone()),
            derivation: Vec::new(),
        },
    };

    let mut symbol_claim = SymbolIdentityClaim {
        symbol: rootlight_ids::SymbolId::from_bytes([0; 20]),
        repository,
        language: "rust".to_owned(),
        kind: EntityKind::Function,
        container: Some(ContainerRef::File(file)),
        container_identity: file.as_bytes().to_vec(),
        declared_identity: "answer".to_owned(),
        signature_discriminator: b"()->u32".to_vec(),
        build_context_discriminator: build_context.digest().as_bytes().to_vec(),
    };
    symbol_claim.symbol = symbol_claim.derived_symbol();
    let entity = EntityRecord {
        id: symbol_claim.symbol,
        repository,
        generation,
        kind: EntityKind::Function,
        language: "rust".to_owned(),
        tier: AnalysisTier::TierB,
        canonical_name: "answer".to_owned(),
        display_name: "answer".to_owned(),
        qualified_name: "crate::answer".to_owned(),
        container: Some(ContainerRef::File(file)),
        visibility: EntityVisibility::Public,
        flags: Vec::new(),
        provenance: provenance.id,
        evidence: FactEvidence {
            source: Some(definition_source.clone()),
            derivation: Vec::new(),
        },
    };

    let confidence = Confidence::new(1_000).expect("fixture confidence is valid");
    let mut definition = OccurrenceRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        file,
        source: definition_source.clone(),
        role: OccurrenceRole::Definition,
        enclosing: Some(entity.id),
        target: OccurrenceTarget::Resolved { symbol: entity.id },
        syntactic_text_hash: content_hash(b"answer"),
        syntax_kind: "function.definition".to_owned(),
        provenance: provenance.id,
        confidence,
        evidence: FactEvidence {
            source: Some(definition_source.clone()),
            derivation: Vec::new(),
        },
    };
    definition.id = derive_occurrence_record_id(&definition).expect("definition identity derives");
    let mut call = OccurrenceRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        file,
        source: call_source.clone(),
        role: OccurrenceRole::CallSite,
        enclosing: Some(entity.id),
        target: OccurrenceTarget::Resolved { symbol: entity.id },
        syntactic_text_hash: content_hash(b"answer"),
        syntax_kind: "call.expression".to_owned(),
        provenance: provenance.id,
        confidence,
        evidence: FactEvidence {
            source: Some(call_source.clone()),
            derivation: Vec::new(),
        },
    };
    call.id = derive_occurrence_record_id(&call).expect("call identity derives");

    let mut definition_relation = RelationRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        subject: RelationEndpoint::Entity(entity.id),
        predicate: RelationPredicate::DefinesAt,
        object: RelationEndpoint::Occurrence(definition.id),
        confidence,
        evidence_kind: EvidenceKind::Syntax,
        provenance: provenance.id,
        evidence: FactEvidence {
            source: Some(definition_source.clone()),
            derivation: Vec::new(),
        },
    };
    definition_relation.id = derive_relation_record_id(&definition_relation)
        .expect("definition relation identity derives");
    let mut call_relation = RelationRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        subject: RelationEndpoint::Entity(entity.id),
        predicate: RelationPredicate::Calls,
        object: RelationEndpoint::Occurrence(call.id),
        confidence,
        evidence_kind: EvidenceKind::Syntax,
        provenance: provenance.id,
        evidence: FactEvidence {
            source: Some(call_source.clone()),
            derivation: Vec::new(),
        },
    };
    call_relation.id =
        derive_relation_record_id(&call_relation).expect("call relation identity derives");

    let mut occurrence_coverage = CoverageRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        scope: CoverageScope::File(file),
        domain: FactDomain::Occurrences,
        tier: AnalysisTier::TierB,
        status: CoverageStatus::Complete,
        discovered: 2,
        indexed: 2,
        skipped: 0,
        provenance: provenance.id,
        evidence: FactEvidence {
            source: Some(full_source.clone()),
            derivation: Vec::new(),
        },
    };
    occurrence_coverage.id = derive_coverage_record_id(&occurrence_coverage)
        .expect("occurrence coverage identity derives");
    let mut relation_coverage = CoverageRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        scope: CoverageScope::File(file),
        domain: FactDomain::Relations,
        tier: AnalysisTier::TierB,
        status: CoverageStatus::Complete,
        discovered: 2,
        indexed: 2,
        skipped: 0,
        provenance: provenance.id,
        evidence: FactEvidence {
            source: Some(full_source.clone()),
            derivation: Vec::new(),
        },
    };
    relation_coverage.id =
        derive_coverage_record_id(&relation_coverage).expect("relation coverage identity derives");

    let file_claim_envelope =
        new_file_identity_claim_envelope(&file_claim, generation, provenance.id, full_source)
            .expect("file claim envelope is valid");
    let symbol_claim_envelope = new_symbol_identity_claim_envelope(
        &symbol_claim,
        generation,
        provenance.id,
        definition_source,
    )
    .expect("symbol claim envelope is valid");

    let document = NormalizedIrDocument {
        version: rootlight_ir::NormalizedIrVersion::new(),
        repository,
        generation,
        files: vec![file_record],
        entities: vec![entity],
        occurrences: vec![definition, call],
        relations: vec![definition_relation, call_relation],
        provenance: vec![provenance],
        source_mappings: Vec::new(),
        coverage_records: vec![occurrence_coverage, relation_coverage],
        skipped_regions: Vec::new(),
        diagnostics: Vec::new(),
        extensions: vec![file_claim_envelope, symbol_claim_envelope],
    };
    let metadata = GenerationMetadata::new(
        repository,
        generation,
        None,
        manifest_hash,
        configuration_hash,
        provider_set_hash,
    )
    .expect("fixture metadata is valid");
    (metadata, document)
}

fn verify(
    metadata: GenerationMetadata,
    document: NormalizedIrDocument,
    context: &GenerationContext<'_>,
) -> IdentityVerifiedGeneration {
    IdentityVerifiedGeneration::verify(
        metadata,
        document,
        &IrLimits::default(),
        &ExtensionSupport::default(),
        context,
    )
    .expect("fixture identities verify")
}

fn readers(
    context: &GenerationContext<'_>,
) -> (EphemeralOracleReader, SegmentReader, NormalizedIrDocument) {
    let (metadata, document) = fixture();
    let oracle = EphemeralOracleWriter::create()
        .expect("ephemeral oracle initializes")
        .seal(verify(metadata, document.clone(), context), context)
        .expect("fixture seals in SQLite");
    let segment = Segment::encode(verify(metadata, document, context), oracle.stats(), context)
        .expect("fixture segment encodes");
    let reader = SegmentReader::open(
        segment.into_bytes(),
        &IrLimits::default(),
        &ExtensionSupport::default(),
        context,
    )
    .expect("fixture segment opens");
    let canonical = oracle
        .read_generation(context)
        .expect("oracle generation reads")
        .into_snapshot()
        .into_document();
    (oracle, reader, canonical)
}

fn encoded_fixture(context: &GenerationContext<'_>) -> Segment {
    let (metadata, document) = fixture();
    let oracle = EphemeralOracleWriter::create()
        .expect("ephemeral oracle initializes")
        .seal(verify(metadata, document.clone(), context), context)
        .expect("fixture seals in SQLite");
    Segment::encode(verify(metadata, document, context), oracle.stats(), context)
        .expect("fixture segment encodes")
}

#[test]
fn segment_reader_matches_sqlite_for_every_indexed_operation() {
    let cancellation = Cancellation::new();
    let context = context(&cancellation);
    let (oracle, segment, document) = readers(&context);

    assert_eq!(segment.metadata(), oracle.metadata());
    assert_eq!(segment.stats(), oracle.stats());
    assert_eq!(
        segment
            .read_generation(&context)
            .expect("segment generation reads")
            .document(),
        oracle
            .read_generation(&context)
            .expect("oracle generation reads")
            .document()
    );
    assert_eq!(
        segment
            .file(document.files[0].id, &context)
            .expect("segment file reads"),
        oracle
            .file(document.files[0].id, &context)
            .expect("oracle file reads")
    );
    assert_eq!(
        segment
            .entity(document.entities[0].id, &context)
            .expect("segment entity reads"),
        oracle
            .entity(document.entities[0].id, &context)
            .expect("oracle entity reads")
    );
    assert_eq!(
        segment
            .provenance(document.provenance[0].id, &context)
            .expect("segment provenance reads"),
        oracle
            .provenance(document.provenance[0].id, &context)
            .expect("oracle provenance reads")
    );
    assert!(
        segment
            .file(rootlight_ids::FileId::from_bytes([0xff; 20]), &context)
            .expect("missing segment file read succeeds")
            .is_none()
    );

    let limit = GenerationReadLimit::new(1).expect("one-item page is valid");
    let relation_request = RelationReadRequest::new(
        document.relations[0].subject,
        RelationReadDirection::Outgoing,
        limit,
    );
    let oracle_relations = oracle
        .relations(&relation_request, &context)
        .expect("oracle relation page reads");
    let segment_relations = segment
        .relations(&relation_request, &context)
        .expect("segment relation page reads");
    assert_eq!(segment_relations, oracle_relations);
    let relation_cursor = oracle_relations
        .next_cursor()
        .expect("first relation page is truncated");
    assert_eq!(
        segment
            .relations(
                &relation_request.clone().with_after(relation_cursor),
                &context,
            )
            .expect("segment second relation page reads"),
        oracle
            .relations(
                &relation_request.clone().with_after(relation_cursor),
                &context
            )
            .expect("oracle second relation page reads")
    );
    let filtered = relation_request
        .clone()
        .with_predicates(vec![document.relations[0].predicate])
        .expect("predicate filter is bounded");
    assert_eq!(
        segment
            .relations(&filtered, &context)
            .expect("segment filtered relations read"),
        oracle
            .relations(&filtered, &context)
            .expect("oracle filtered relations read")
    );
    let incoming = RelationReadRequest::new(
        document.relations[0].object,
        RelationReadDirection::Incoming,
        limit,
    );
    assert_eq!(
        segment
            .relations(&incoming, &context)
            .expect("segment incoming relations read"),
        oracle
            .relations(&incoming, &context)
            .expect("oracle incoming relations read")
    );

    let occurrence_request = OccurrenceReadRequest::new(document.occurrences[0].file, limit);
    let oracle_occurrences = oracle
        .occurrences(&occurrence_request, &context)
        .expect("oracle occurrence page reads");
    assert_eq!(
        segment
            .occurrences(&occurrence_request, &context)
            .expect("segment occurrence page reads"),
        oracle_occurrences
    );
    let occurrence_cursor = oracle_occurrences
        .next_cursor()
        .expect("first occurrence page is truncated");
    assert_eq!(
        segment
            .occurrences(&occurrence_request.with_after(occurrence_cursor), &context,)
            .expect("segment second occurrence page reads"),
        oracle
            .occurrences(&occurrence_request.with_after(occurrence_cursor), &context,)
            .expect("oracle second occurrence page reads")
    );

    let coverage_request = CoverageReadRequest::new(document.coverage_records[0].scope, limit);
    let oracle_coverage = oracle
        .coverage(&coverage_request, &context)
        .expect("oracle coverage page reads");
    assert_eq!(
        segment
            .coverage(&coverage_request, &context)
            .expect("segment coverage page reads"),
        oracle_coverage
    );
    let coverage_cursor = oracle_coverage
        .next_cursor()
        .expect("first coverage page is truncated");
    assert_eq!(
        segment
            .coverage(&coverage_request.with_after(coverage_cursor), &context)
            .expect("segment second coverage page reads"),
        oracle
            .coverage(&coverage_request.with_after(coverage_cursor), &context)
            .expect("oracle second coverage page reads")
    );
}

#[test]
fn segment_encoding_is_deterministic() {
    let cancellation = Cancellation::new();
    let context = context(&cancellation);
    let (metadata, document) = fixture();
    let oracle = EphemeralOracleWriter::create()
        .expect("ephemeral oracle initializes")
        .seal(verify(metadata, document.clone(), &context), &context)
        .expect("fixture seals in SQLite");
    let first = Segment::encode(
        verify(metadata, document.clone(), &context),
        oracle.stats(),
        &context,
    )
    .expect("first segment encodes");
    let second = Segment::encode(
        verify(metadata, document, &context),
        oracle.stats(),
        &context,
    )
    .expect("second segment encodes");

    assert_eq!(first.as_bytes(), second.as_bytes());
}

#[test]
fn decoder_rejects_truncation_bit_flips_bounds_counts_and_divergent_indexes() {
    let cancellation = Cancellation::new();
    let context = context(&cancellation);
    let valid = encoded_fixture(&context);

    let mut truncated = valid.as_bytes().to_vec();
    truncated.pop();
    assert_corrupt(truncated, &context);

    let mut flipped = valid.as_bytes().to_vec();
    let last = flipped.len() - 1;
    flipped[last] ^= 0x80;
    assert_corrupt(flipped, &context);

    let mut overlapping = valid.as_bytes().to_vec();
    write_u64(&mut overlapping, DESCRIPTOR_START + 8, 575);
    refresh_header_checksum(&mut overlapping);
    assert_corrupt(overlapping, &context);

    let mut oversized_count = valid.as_bytes().to_vec();
    let files_descriptor = DESCRIPTOR_START + 2 * DESCRIPTOR_BYTES;
    write_u64(&mut oversized_count, files_descriptor + 24, u64::MAX);
    refresh_header_checksum(&mut oversized_count);
    assert_corrupt(oversized_count, &context);

    let mut divergent_index = valid.as_bytes().to_vec();
    let index_offset = read_u64(&divergent_index, files_descriptor + 8);
    let index_length = read_u64(&divergent_index, files_descriptor + 16);
    let start = usize::try_from(index_offset).expect("fixture offset fits usize");
    let end = start + usize::try_from(index_length).expect("fixture length fits usize");
    divergent_index[start] ^= 1;
    let checksum = blake3::hash(&divergent_index[start..end]);
    divergent_index[files_descriptor + 32..files_descriptor + 64]
        .copy_from_slice(checksum.as_bytes());
    refresh_header_checksum(&mut divergent_index);
    assert_corrupt(divergent_index, &context);
}

#[test]
fn decoder_reports_an_explicit_unsupported_version() {
    let cancellation = Cancellation::new();
    let context = context(&cancellation);
    let mut bytes = encoded_fixture(&context).as_bytes().to_vec();
    bytes[8..10].copy_from_slice(&9_u16.to_le_bytes());
    refresh_header_checksum(&mut bytes);

    let error = SegmentReader::open(
        Arc::from(bytes.into_boxed_slice()),
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &context,
    )
    .expect_err("unsupported format is rejected");
    assert!(matches!(
        error,
        SegmentError::UnsupportedVersion { major: 9, minor: 0 }
    ));
}

#[test]
fn encoding_open_and_indexed_reads_honor_cancellation() {
    let active = Cancellation::new();
    let active_context = context(&active);
    let valid = encoded_fixture(&active_context);
    let reader = SegmentReader::open(
        valid.bytes(),
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &active_context,
    )
    .expect("fixture segment opens");
    let document = reader
        .read_generation(&active_context)
        .expect("fixture generation reads")
        .into_snapshot()
        .into_document();

    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    let cancelled_context = context(&cancelled);
    let (metadata, unverified_document) = fixture();
    let generation = verify(metadata, unverified_document, &active_context);
    assert!(matches!(
        Segment::encode(generation, reader.stats(), &cancelled_context),
        Err(SegmentError::Control(_))
    ));
    assert!(matches!(
        SegmentReader::open(
            valid.bytes(),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &cancelled_context,
        ),
        Err(SegmentError::Control(_))
    ));
    let request = RelationReadRequest::new(
        document.relations[0].subject,
        RelationReadDirection::Outgoing,
        GenerationReadLimit::default(),
    );
    assert!(matches!(
        reader.relations(&request, &cancelled_context),
        Err(SegmentError::Control(_))
    ));
}

fn assert_corrupt(bytes: Vec<u8>, context: &GenerationContext<'_>) {
    let error = SegmentReader::open(
        Arc::from(bytes.into_boxed_slice()),
        &IrLimits::default(),
        &ExtensionSupport::default(),
        context,
    )
    .expect_err("corrupt segment is rejected");
    assert!(matches!(error, SegmentError::Corrupt));
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("fixture field has eight bytes"),
    )
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn refresh_header_checksum(bytes: &mut [u8]) {
    bytes[HEADER_CHECKSUM_START..HEADER_CHECKSUM_END].fill(0);
    let checksum = blake3::hash(&bytes[..HEADER_BYTES]);
    bytes[HEADER_CHECKSUM_START..HEADER_CHECKSUM_END].copy_from_slice(checksum.as_bytes());
}
