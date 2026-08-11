//! Public-boundary tests for bounded official SCIP protobuf export.
//!
//! Fixtures cover deterministic ordering, exact source identity and UTF-8
//! ranges, documented omission accounting, hard limits, and cancellation.

use protobuf::{EnumOrUnknown, Message, MessageField};
use rootlight_adapters::{
    SCIP_EXPORT_SUBSET_VERSION, ScipExportError, ScipExportLimits, ScipExportRequest,
    ScipExportResource, ScipExportSource, ScipImportRequest, ScipImportSource, export_scip_index,
    import_scip_index,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{
    FactId, FileIdentity, GenerationId, SymbolId, content_hash, derive_file, derive_repository,
};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, Confidence, ContainerRef, CoverageStatus, EntityKind,
    EntityRecord, EntityVisibility, EvidenceKind, ExtensionSupport, FactEvidence, FileRecord,
    IrLimits, NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, ProducerIdentity,
    ProducerKind, ProvenanceRecord, RelationEndpoint, RelationPredicate, RelationRecord, SourceRef,
    SourceSpan, SymbolIdentityClaim, derive_occurrence_record_id, derive_provenance_record_id,
    derive_relation_record_id, new_symbol_identity_claim_envelope,
};
use scip::types::{
    Document, Index, Metadata, MultiLineRange, PositionEncoding, Relationship, SingleLineRange,
    SymbolInformation, TextEncoding, ToolInfo, occurrence::Typed_range, symbol_information::Kind,
};

const SOURCE_A: &str = "const PRIVATE: &str = \"C:\\Users\\sample\\private\";\nfn 🚀target() {}\nfn caller() {\n    🚀target();\n}\n";
const SOURCE_Z: &str = "fn zeta() {}\n";
const TARGET: &str = "rust cargo fixture 0.1.0 target().";
const CALLER: &str = "rust cargo fixture 0.1.0 caller().";
const ZETA: &str = "rust cargo fixture 0.1.0 zeta().";

#[test]
fn export_is_deterministic_parseable_and_canonically_ordered() {
    let document = fixture_document();
    let sources = export_sources(&document);
    let first = export(&document, &sources).expect("fixture IR exports");

    let mut reordered = document.clone();
    reordered.files.reverse();
    reordered.entities.reverse();
    reordered.occurrences.reverse();
    reordered.relations.reverse();
    reordered.provenance.reverse();
    reordered.coverage_records.reverse();
    reordered.extensions.reverse();
    let mut reordered_sources = export_sources(&reordered);
    reordered_sources.reverse();
    let second = export(&reordered, &reordered_sources).expect("reordered fixture IR exports");

    assert_eq!(first, second);
    assert_eq!(first.report().subset_version(), SCIP_EXPORT_SUBSET_VERSION);
    assert_eq!(first.report().documents(), 2);
    assert_eq!(first.report().symbols(), 3);
    assert_eq!(first.report().occurrences(), 5);
    assert_eq!(first.report().relationships(), 1);
    assert_eq!(first.report().encoded_bytes(), first.encoded().len());
    assert_eq!(first.report().omissions().entity_metadata(), 3);
    assert_eq!(first.report().omissions().occurrence_metadata(), 5);
    assert_eq!(
        first.report().omissions().provenance(),
        document.provenance.len()
    );
    assert_eq!(
        first.report().omissions().coverage_records(),
        document.coverage_records.len()
    );
    assert_eq!(
        first.report().omissions().extensions(),
        document.extensions.len()
    );

    let index = Index::parse_from_bytes(first.encoded()).expect("SCIP output parses");
    let metadata = index.metadata.as_ref().expect("metadata is present");
    let tool = metadata
        .tool_info
        .as_ref()
        .expect("tool identity is present");
    assert_eq!(tool.name, "rootlight-adapters");
    assert_eq!(tool.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        tool.arguments,
        [format!("subset={SCIP_EXPORT_SUBSET_VERSION}")]
    );
    assert!(metadata.project_root.is_empty());
    assert_eq!(
        metadata
            .text_document_encoding
            .enum_value()
            .expect("text encoding is known"),
        TextEncoding::UTF8
    );
    assert_eq!(
        index
            .documents
            .iter()
            .map(|document| document.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["src/a.rs", "src/z.rs"]
    );
    assert!(
        index
            .documents
            .iter()
            .all(|document| document.text.is_empty())
    );
    for document in &index.documents {
        assert!(document.symbols.is_sorted_by_key(|symbol| &symbol.symbol));
        assert!(
            document
                .symbols
                .iter()
                .all(|symbol| scip::symbol::parse_symbol(&symbol.symbol).is_ok())
        );
        assert!(occurrences_are_sorted(&document.occurrences));
    }
}

#[test]
fn definitions_resolved_references_and_supported_relationships_are_preserved() {
    let mut document = fixture_document();
    let target = entity_id(&document, "target");
    let caller = entity_id(&document, "caller");
    for predicate in [
        RelationPredicate::Implements,
        RelationPredicate::UsesType,
        RelationPredicate::BindsTo,
    ] {
        add_relation(&mut document, caller, target, predicate);
    }
    let sources = export_sources(&document);
    let outcome = export(&document, &sources).expect("fixture IR exports");
    let index = Index::parse_from_bytes(outcome.encoded()).expect("SCIP output parses");
    let all_symbols = index
        .documents
        .iter()
        .flat_map(|document| document.symbols.iter())
        .map(|information| information.symbol.as_str())
        .collect::<Vec<_>>();
    let occurrences = index
        .documents
        .iter()
        .flat_map(|document| document.occurrences.iter())
        .collect::<Vec<_>>();

    assert_eq!(
        occurrences
            .iter()
            .filter(|occurrence| occurrence.symbol_roles & 0x1 != 0)
            .count(),
        3
    );
    assert!(
        occurrences
            .iter()
            .all(|occurrence| all_symbols.contains(&occurrence.symbol.as_str()))
    );
    assert!(
        occurrences
            .iter()
            .any(|occurrence| occurrence.symbol_roles == 0)
    );
    let relationships = index
        .documents
        .iter()
        .flat_map(|document| document.symbols.iter())
        .flat_map(|symbol| symbol.relationships.iter())
        .collect::<Vec<_>>();
    assert_eq!(relationships.len(), 1);
    assert!(relationships[0].is_reference);
    assert!(relationships[0].is_implementation);
    assert!(relationships[0].is_type_definition);
    assert!(relationships[0].is_definition);
    assert!(all_symbols.contains(&relationships[0].symbol.as_str()));
}

#[test]
fn multiline_and_non_ascii_byte_spans_use_exact_utf8_positions() {
    let document = fixture_document();
    let sources = export_sources(&document);
    let outcome = export(&document, &sources).expect("fixture IR exports");
    let index = Index::parse_from_bytes(outcome.encoded()).expect("SCIP output parses");
    let document = index
        .documents
        .iter()
        .find(|document| document.relative_path == "src/a.rs")
        .expect("source document is exported");

    assert!(document.occurrences.iter().any(|occurrence| {
        matches!(
            occurrence.typed_range.as_ref(),
            Some(Typed_range::SingleLineRange(range))
                if range.line == 1
                    && range.start_character == 3
                    && range.end_character == 13
        )
    }));
    assert!(document.occurrences.iter().any(|occurrence| {
        matches!(
            occurrence.typed_range.as_ref(),
            Some(Typed_range::MultiLineRange(range))
                if range.start_line == 2
                    && range.start_character == 3
                    && range.end_line == 3
                    && range.end_character == 14
        )
    }));
}

#[test]
fn unsupported_and_ambiguous_facts_are_counted_without_inventing_certainty() {
    let mut document = fixture_document();
    let target = entity_id(&document, "target");
    let caller = entity_id(&document, "caller");
    let base = document
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .expect("reference occurrence exists")
        .clone();

    let mut ambiguous = base.clone();
    let mut candidates = vec![target, caller];
    candidates.sort();
    ambiguous.target = OccurrenceTarget::Candidates {
        symbols: candidates,
        total_count: 2,
        completeness: CoverageStatus::Complete,
    };
    ambiguous.id =
        derive_occurrence_record_id(&ambiguous).expect("ambiguous occurrence identity derives");
    document.occurrences.push(ambiguous);

    let mut unresolved = base.clone();
    unresolved.target = OccurrenceTarget::Unresolved {
        text_hash: content_hash(b"unknown"),
    };
    unresolved.id =
        derive_occurrence_record_id(&unresolved).expect("unresolved occurrence identity derives");
    document.occurrences.push(unresolved);

    let mut inexact = base;
    inexact.confidence = Confidence::new(999).expect("fixture confidence is valid");
    inexact.id =
        derive_occurrence_record_id(&inexact).expect("inexact occurrence identity derives");
    document.occurrences.push(inexact);

    add_unsupported_entity(&mut document);
    add_unsupported_relation(&mut document, caller, target);

    let sources = export_sources(&document);
    let outcome = export(&document, &sources).expect("bounded omissions remain exportable");
    let omissions = outcome.report().omissions();
    assert_eq!(omissions.unsupported_entities(), 1);
    assert_eq!(omissions.ambiguous_occurrences(), 1);
    assert_eq!(omissions.unresolved_occurrences(), 1);
    assert_eq!(omissions.inexact_occurrences(), 1);
    assert_eq!(omissions.unsupported_relationships(), 1);
    assert_eq!(outcome.report().symbols(), 3);
    assert_eq!(outcome.report().occurrences(), 5);
    assert_eq!(outcome.report().relationships(), 1);
}

#[test]
fn exact_source_binding_and_tampering_are_rejected_source_free() {
    let document = fixture_document();
    let sources = export_sources(&document);
    let original = sources[0];

    let missing = export(&document, &sources[1..]);
    assert!(matches!(missing, Err(ScipExportError::SourceSetMismatch)));

    let mut tampered = sources.clone();
    tampered[0] = ScipExportSource::new(
        original.repository(),
        original.generation(),
        original.file(),
        original.path(),
        b"different",
    );
    assert!(matches!(
        export(&document, &tampered),
        Err(ScipExportError::SourceMismatch)
    ));

    let mut wrong_generation = sources.clone();
    wrong_generation[0] = ScipExportSource::new(
        original.repository(),
        GenerationId::from_bytes([9; 20]),
        original.file(),
        original.path(),
        original.content(),
    );
    assert!(matches!(
        export(&document, &wrong_generation),
        Err(ScipExportError::SourceIdentityMismatch)
    ));

    let mut absolute = sources;
    absolute[0] = ScipExportSource::new(
        original.repository(),
        original.generation(),
        original.file(),
        "C:/Users/sample/private.rs",
        original.content(),
    );
    let error = export(&document, &absolute).expect_err("absolute paths are rejected");
    assert!(matches!(error, ScipExportError::InvalidPath));
    assert!(!error.to_string().contains("sample"));
}

#[test]
fn malformed_utf8_boundary_ranges_are_rejected() {
    let mut document = fixture_document();
    let target = entity_id(&document, "target");
    let occurrence = document
        .occurrences
        .iter_mut()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && matches!(
                    occurrence.target,
                    OccurrenceTarget::Resolved { symbol } if symbol == target
                )
        })
        .expect("definition occurrence exists");
    let old = occurrence.source.clone();
    let span = old.span();
    let start = span.start_byte() + 1;
    let new_span =
        SourceSpan::new(span.file(), start, span.end_byte()).expect("byte span remains ordered");
    let source = SourceRef::new(
        old.repository(),
        old.generation(),
        new_span,
        old.content_hash(),
        None,
    );
    occurrence.source = source.clone();
    occurrence.evidence.source = Some(source);
    let bytes = &SOURCE_A.as_bytes()[usize::try_from(start).expect("fixture offset fits")
        ..usize::try_from(span.end_byte()).expect("fixture offset fits")];
    occurrence.syntactic_text_hash = content_hash(bytes);
    occurrence.id =
        derive_occurrence_record_id(occurrence).expect("tampered occurrence identity derives");

    let sources = export_sources(&document);
    assert!(matches!(
        export(&document, &sources),
        Err(ScipExportError::InvalidRange)
    ));
}

#[test]
fn hard_limits_and_cancellation_stop_export() {
    let document = fixture_document();
    let sources = export_sources(&document);
    let defaults = ScipExportLimits::default();
    let encoded_limit = ScipExportLimits::new(
        1,
        defaults.max_documents(),
        defaults.max_symbols(),
        defaults.max_occurrences(),
        defaults.max_relationships(),
        defaults.max_source_bytes(),
        defaults.max_total_source_bytes(),
    )
    .expect("narrow encoded limit is valid");
    let error = export_with(&document, &sources, encoded_limit, &Cancellation::new())
        .expect_err("encoded output exceeds one byte");
    assert!(matches!(
        error,
        ScipExportError::LimitExceeded {
            resource: ScipExportResource::EncodedBytes,
            ..
        }
    ));

    let symbol_limit = ScipExportLimits::new(
        defaults.max_encoded_bytes(),
        defaults.max_documents(),
        1,
        defaults.max_occurrences(),
        defaults.max_relationships(),
        defaults.max_source_bytes(),
        defaults.max_total_source_bytes(),
    )
    .expect("narrow symbol limit is valid");
    let error = export_with(&document, &sources, symbol_limit, &Cancellation::new())
        .expect_err("input entities exceed one symbol");
    assert!(matches!(
        error,
        ScipExportError::LimitExceeded {
            resource: ScipExportResource::Symbols,
            ..
        }
    ));

    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        export_with(&document, &sources, defaults, &cancellation),
        Err(ScipExportError::Cancelled)
    ));
}

#[test]
fn export_bounds_line_index_memory_before_allocation() {
    let source = vec![b'\n'; 1_000_000];
    let repository = derive_repository(b"line-dense-scip-export").id();
    let generation = GenerationId::from_bytes([31; 20]);
    let path = "src/line-dense.rs";
    let file = derive_file(FileIdentity {
        repository,
        path_identity: path.as_bytes(),
    })
    .id();
    let source_ref = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(
            file,
            0,
            u64::try_from(source.len()).expect("fixture length fits u64"),
        )
        .expect("fixture source span is valid"),
        content_hash(&source),
        None,
    );
    let build_context = BuildContextIdentity::new(content_hash(b"line-dense-build"));
    let mut provenance = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        producer_kind: ProducerKind::Scip,
        producer: ProducerIdentity::new("fixture-scip", "1.0", build_context.digest())
            .expect("producer identity is valid"),
        binary_digest: content_hash(b"fixture-scip-binary"),
        frontend_version: Some("fixture-scip-1".to_owned()),
        language: "rust".to_owned(),
        tier: AnalysisTier::TierB,
        build_context,
        input_sources: vec![source_ref.clone()],
        evidence_sources: vec![source_ref.clone()],
        derivation_parents: Vec::new(),
        rule: None,
    };
    provenance.id = derive_provenance_record_id(&provenance).expect("provenance identity derives");
    let mut document = NormalizedIrDocument::empty(repository, generation);
    document.files.push(FileRecord {
        id: file,
        repository,
        generation,
        path: path.to_owned(),
        path_locator: None,
        content_hash: content_hash(&source),
        byte_length: u64::try_from(source.len()).expect("fixture length fits u64"),
        language: "rust".to_owned(),
        encoding: "utf-8".to_owned(),
        generated: false,
        provenance: provenance.id,
        evidence: FactEvidence {
            source: Some(source_ref),
            derivation: Vec::new(),
        },
    });
    document.provenance.push(provenance);
    let sources = [ScipExportSource::new(
        repository, generation, file, path, &source,
    )];

    let error = export(&document, &sources).expect_err("line index ceiling rejects amplification");
    assert!(matches!(
        error,
        ScipExportError::LimitExceeded {
            resource: ScipExportResource::LineStarts,
            observed: 1_000_001,
            limit: 1_000_000,
        }
    ));
}

#[test]
fn output_never_contains_project_root_source_text_or_private_host_path() {
    let document = fixture_document();
    let sources = export_sources(&document);
    let outcome = export(&document, &sources).expect("fixture IR exports");

    assert!(!contains(outcome.encoded(), b"file:///"));
    assert!(!contains(outcome.encoded(), b"C:\\Users\\sample"));
    assert!(!contains(outcome.encoded(), b"PRIVATE"));
    assert!(contains(outcome.encoded(), b"src/a.rs"));
}

fn export<'a>(
    document: &rootlight_ir::NormalizedIrDocument,
    sources: &'a [ScipExportSource<'a>],
) -> Result<rootlight_adapters::ScipExportOutcome, ScipExportError> {
    export_with(
        document,
        sources,
        ScipExportLimits::default(),
        &Cancellation::new(),
    )
}

fn export_with<'a>(
    document: &rootlight_ir::NormalizedIrDocument,
    sources: &'a [ScipExportSource<'a>],
    limits: ScipExportLimits,
    cancellation: &Cancellation,
) -> Result<rootlight_adapters::ScipExportOutcome, ScipExportError> {
    let ir_limits = IrLimits::default();
    let extensions = ExtensionSupport::default();
    export_scip_index(
        document,
        ScipExportRequest::new(
            document.repository,
            document.generation,
            sources,
            &ir_limits,
            &extensions,
            cancellation,
        )
        .with_limits(limits),
    )
}

fn fixture_document() -> rootlight_ir::NormalizedIrDocument {
    let index = fixture_index()
        .write_to_bytes()
        .expect("fixture SCIP index encodes");
    let repository = derive_repository(b"scip-export-fixture").id();
    let generation = GenerationId::from_bytes([7; 20]);
    let build_context = BuildContextIdentity::new(content_hash(b"scip-export-build"));
    let sources = [
        ScipImportSource::new("src/a.rs", SOURCE_A.as_bytes(), false),
        ScipImportSource::new("src/z.rs", SOURCE_Z.as_bytes(), false),
    ];
    let ir_limits = IrLimits::default();
    let cancellation = Cancellation::new();
    import_scip_index(
        &index,
        ScipImportRequest::new(
            repository,
            generation,
            build_context,
            &sources,
            &ir_limits,
            &cancellation,
        ),
    )
    .expect("fixture SCIP index imports")
    .into_document()
}

fn export_sources(document: &rootlight_ir::NormalizedIrDocument) -> Vec<ScipExportSource<'static>> {
    [("src/a.rs", SOURCE_A), ("src/z.rs", SOURCE_Z)]
        .into_iter()
        .map(|(path, content)| {
            let file = document
                .files
                .iter()
                .find(|file| file.path == path)
                .expect("fixture file exists");
            ScipExportSource::new(
                document.repository,
                document.generation,
                file.id,
                path,
                content.as_bytes(),
            )
        })
        .collect()
}

fn fixture_index() -> Index {
    let mut metadata = Metadata {
        text_document_encoding: EnumOrUnknown::new(TextEncoding::UTF8),
        project_root: "file:///must-not-survive".to_owned(),
        ..Default::default()
    };
    metadata.tool_info = MessageField::some(ToolInfo {
        name: "fixture-indexer".to_owned(),
        version: "1.0.0".to_owned(),
        ..Default::default()
    });
    Index {
        metadata: MessageField::some(metadata),
        documents: vec![z_document(), a_document()],
        ..Default::default()
    }
}

fn a_document() -> Document {
    let mut caller = symbol(CALLER, "caller");
    caller.relationships.push(Relationship {
        symbol: TARGET.to_owned(),
        is_reference: true,
        ..Default::default()
    });
    Document {
        language: "Rust".to_owned(),
        relative_path: "src/a.rs".to_owned(),
        occurrences: vec![
            single_occurrence(TARGET, 1, 3, 13, 0x1),
            single_occurrence(CALLER, 2, 3, 9, 0x1),
            single_occurrence(TARGET, 3, 4, 14, 0),
            multi_occurrence(TARGET, 2, 3, 3, 14),
        ],
        symbols: vec![symbol(TARGET, "target"), caller],
        text: SOURCE_A.to_owned(),
        position_encoding: EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart),
        ..Default::default()
    }
}

fn z_document() -> Document {
    Document {
        language: "Rust".to_owned(),
        relative_path: "src/z.rs".to_owned(),
        occurrences: vec![single_occurrence(ZETA, 0, 3, 7, 0x1)],
        symbols: vec![symbol(ZETA, "zeta")],
        text: SOURCE_Z.to_owned(),
        position_encoding: EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart),
        ..Default::default()
    }
}

fn symbol(identity: &str, display_name: &str) -> SymbolInformation {
    SymbolInformation {
        symbol: identity.to_owned(),
        kind: EnumOrUnknown::new(Kind::Function),
        display_name: display_name.to_owned(),
        ..Default::default()
    }
}

fn single_occurrence(
    symbol: &str,
    line: i32,
    start_character: i32,
    end_character: i32,
    roles: i32,
) -> scip::types::Occurrence {
    let mut occurrence = scip::types::Occurrence {
        symbol: symbol.to_owned(),
        symbol_roles: roles,
        ..Default::default()
    };
    occurrence.set_single_line_range(SingleLineRange {
        line,
        start_character,
        end_character,
        ..Default::default()
    });
    occurrence
}

fn multi_occurrence(
    symbol: &str,
    start_line: i32,
    start_character: i32,
    end_line: i32,
    end_character: i32,
) -> scip::types::Occurrence {
    let mut occurrence = scip::types::Occurrence {
        symbol: symbol.to_owned(),
        ..Default::default()
    };
    occurrence.set_multi_line_range(MultiLineRange {
        start_line,
        start_character,
        end_line,
        end_character,
        ..Default::default()
    });
    occurrence
}

fn entity_id(document: &rootlight_ir::NormalizedIrDocument, name: &str) -> SymbolId {
    document
        .entities
        .iter()
        .find(|entity| entity.display_name == name)
        .expect("fixture entity exists")
        .id
}

fn add_unsupported_entity(document: &mut rootlight_ir::NormalizedIrDocument) {
    let file = document
        .files
        .iter()
        .find(|file| file.path == "src/a.rs")
        .expect("fixture file exists");
    let source = file
        .evidence
        .source
        .as_ref()
        .expect("fixture file has source evidence")
        .clone();
    let build_context = document
        .provenance
        .iter()
        .find(|record| record.id == file.provenance)
        .expect("fixture provenance exists")
        .build_context;
    let mut container_identity = Vec::with_capacity(1 + file.id.as_bytes().len());
    container_identity.push(1);
    container_identity.extend_from_slice(file.id.as_bytes());
    let mut claim = SymbolIdentityClaim {
        symbol: SymbolId::from_bytes([0; 20]),
        repository: document.repository,
        language: file.language.clone(),
        kind: EntityKind::Route,
        container: Some(ContainerRef::File(file.id)),
        container_identity,
        declared_identity: "fixture-route".to_owned(),
        signature_discriminator: Vec::new(),
        build_context_discriminator: build_context.digest().as_bytes().to_vec(),
    };
    claim.symbol = claim.derived_symbol();
    document.entities.push(EntityRecord {
        id: claim.symbol,
        repository: document.repository,
        generation: document.generation,
        kind: EntityKind::Route,
        language: file.language.clone(),
        tier: rootlight_ir::AnalysisTier::TierB,
        canonical_name: "fixture-route".to_owned(),
        display_name: "fixture-route".to_owned(),
        qualified_name: "fixture-route".to_owned(),
        container: claim.container,
        visibility: EntityVisibility::Unknown,
        flags: Vec::new(),
        provenance: file.provenance,
        evidence: FactEvidence {
            source: Some(source.clone()),
            derivation: Vec::new(),
        },
    });
    document.extensions.push(
        new_symbol_identity_claim_envelope(&claim, document.generation, file.provenance, source)
            .expect("unsupported symbol identity envelope encodes"),
    );
}

fn add_unsupported_relation(
    document: &mut rootlight_ir::NormalizedIrDocument,
    subject: SymbolId,
    object: SymbolId,
) {
    add_relation(document, subject, object, RelationPredicate::Calls);
}

fn add_relation(
    document: &mut rootlight_ir::NormalizedIrDocument,
    subject: SymbolId,
    object: SymbolId,
    predicate: RelationPredicate,
) {
    let source = document
        .entities
        .iter()
        .find(|entity| entity.id == subject)
        .and_then(|entity| entity.evidence.source.clone())
        .expect("fixture entity has source evidence");
    let provenance = document
        .entities
        .iter()
        .find(|entity| entity.id == subject)
        .expect("fixture entity exists")
        .provenance;
    let mut relation = RelationRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        subject: RelationEndpoint::Entity(subject),
        predicate,
        object: RelationEndpoint::Entity(object),
        confidence: Confidence::new(1_000).expect("fixture confidence is valid"),
        evidence_kind: EvidenceKind::Scip,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: Vec::new(),
        },
    };
    relation.id = derive_relation_record_id(&relation).expect("fixture relation identity derives");
    document.relations.push(relation);
}

fn occurrences_are_sorted(occurrences: &[scip::types::Occurrence]) -> bool {
    occurrences
        .windows(2)
        .all(|pair| occurrence_key(&pair[0]) <= occurrence_key(&pair[1]))
}

fn occurrence_key(occurrence: &scip::types::Occurrence) -> (i32, i32, i32, i32, &str, i32) {
    let range = match occurrence.typed_range.as_ref() {
        Some(Typed_range::SingleLineRange(range)) => (
            range.line,
            range.start_character,
            range.line,
            range.end_character,
        ),
        Some(Typed_range::MultiLineRange(range)) => (
            range.start_line,
            range.start_character,
            range.end_line,
            range.end_character,
        ),
        Some(_) | None => (i32::MAX, i32::MAX, i32::MAX, i32::MAX),
    };
    (
        range.0,
        range.1,
        range.2,
        range.3,
        occurrence.symbol.as_str(),
        occurrence.symbol_roles,
    )
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
