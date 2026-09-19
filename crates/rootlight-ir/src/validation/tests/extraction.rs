//! Container dependencies remain distinct from bytewise source mappings.
//! These authored facts exercise versioning and ownership without Office fixtures.

use super::*;
use crate::{
    FactEvidence, NormalizedIrVersion, SourceMappingKind, SourceMappingRecord, SourceSpan,
};

fn extraction_document() -> NormalizedIrDocument {
    let mut document = fixture();
    document.version = NormalizedIrVersion::V1_6;
    let source = source_for(
        &document,
        document.files[0].id,
        document.files[0].content_hash,
        document.files[0].byte_length,
    );
    let mut container = document.files[0].clone();
    container.id = FileId::from_bytes([241; 20]);
    container.path = "source.container".to_owned();
    container.path_locator = None;
    container.content_hash = rootlight_ids::content_hash(b"opaque container snapshot");
    container.byte_length = 4096;
    let origin = source_for(
        &document,
        container.id,
        container.content_hash,
        container.byte_length,
    );
    container.evidence.source = Some(origin.clone());
    document.files.push(container);
    document.source_mappings = vec![SourceMappingRecord {
        id: FactId::from_bytes([242; 20]),
        repository: document.repository,
        generation: document.generation,
        from: source,
        to: origin.clone(),
        kind: SourceMappingKind::ExtractedFromContainer,
        provenance: document.provenance[0].id,
        evidence: FactEvidence {
            source: Some(origin),
            derivation: Vec::new(),
        },
    }];
    document
}

fn validate(document: &NormalizedIrDocument) -> Result<(), IrDocumentValidationError> {
    validate_ir_document(document, &IrLimits::default(), &ExtensionSupport::default())
}

#[test]
fn extraction_roundtrip_keeps_distinct_hashes_and_non_equivalent_lengths() {
    let document = canonical(extraction_document());
    let encoded = serde_json::to_vec(&document).unwrap();
    let IrDocument::NormalizedV1_6(decoded) =
        decode_ir_document(&encoded, &IrLimits::default(), &ExtensionSupport::default()).unwrap()
    else {
        panic!("extraction requires explicit 1.6 dispatch");
    };
    assert_eq!(decoded, document);
    let mapping = &decoded.source_mappings[0];
    assert_ne!(mapping.from.content_hash(), mapping.to.content_hash());
    assert_ne!(mapping.from.span().end_byte(), mapping.to.span().end_byte());
    let mut other_kind = mapping.clone();
    other_kind.kind = SourceMappingKind::GeneratedToOrigin;
    assert_ne!(
        crate::derive_source_mapping_record_id(mapping).unwrap(),
        crate::derive_source_mapping_record_id(&other_kind).unwrap()
    );
}

#[test]
fn extraction_rejects_every_older_version_without_changing_legacy_mappings() {
    for version in [
        NormalizedIrVersion::V1_1,
        NormalizedIrVersion::V1_2,
        NormalizedIrVersion::V1_3,
        NormalizedIrVersion::V1_4,
        NormalizedIrVersion::V1_5,
    ] {
        let mut document = extraction_document();
        document.version = version;
        assert!(matches!(
            validate(&document),
            Err(IrDocumentValidationError::ExtractionVersion(_))
        ));
        document.source_mappings[0].kind = SourceMappingKind::GeneratedToOrigin;
        validate(&document).unwrap();
    }
}

#[test]
fn extraction_rejects_partial_endpoints_and_foreign_snapshots() {
    for origin in [false, true] {
        for defect in ["partial", "hash", "generation", "repository"] {
            let mut document = extraction_document();
            let mapping = &mut document.source_mappings[0];
            let reference = if origin {
                &mut mapping.to
            } else {
                &mut mapping.from
            };
            *reference = SourceRef::new(
                if defect == "repository" {
                    RepositoryId::from_bytes([249; 16])
                } else {
                    reference.repository()
                },
                if defect == "generation" {
                    GenerationId::from_bytes([248; 20])
                } else {
                    reference.generation()
                },
                SourceSpan::new(
                    reference.span().file(),
                    u64::from(defect == "partial"),
                    reference.span().end_byte(),
                )
                .unwrap(),
                if defect == "hash" {
                    rootlight_ids::content_hash(b"different")
                } else {
                    reference.content_hash()
                },
                None,
            );
            let error = validate(&document).unwrap_err();
            assert!(
                match defect {
                    "partial" => matches!(
                        error,
                        IrDocumentValidationError::PartialExtractionDependency(_)
                    ),
                    "hash" => matches!(error, IrDocumentValidationError::SourceHashMismatch(_)),
                    "generation" =>
                        matches!(error, IrDocumentValidationError::GenerationMismatch { .. }),
                    "repository" =>
                        matches!(error, IrDocumentValidationError::RepositoryMismatch { .. }),
                    _ => unreachable!(),
                },
                "{defect}: {error:?}"
            );
        }
    }
}

#[test]
fn extraction_rejects_multiple_owners_and_cycles() {
    let mut duplicate = extraction_document();
    let mut second = duplicate.source_mappings[0].clone();
    second.id = FactId::from_bytes([243; 20]);
    duplicate.source_mappings.push(second);
    assert!(matches!(
        validate(&duplicate),
        Err(IrDocumentValidationError::DuplicateExtractionOrigin(_))
    ));

    let mut cycle = extraction_document();
    let mut reverse = cycle.source_mappings[0].clone();
    reverse.id = FactId::from_bytes([244; 20]);
    std::mem::swap(&mut reverse.from, &mut reverse.to);
    cycle.source_mappings.push(reverse);
    assert!(matches!(
        validate(&cycle),
        Err(IrDocumentValidationError::ExtractionCycle(_))
    ));

    let mut self_cycle = extraction_document();
    self_cycle.source_mappings[0].to = self_cycle.source_mappings[0].from.clone();
    assert!(matches!(
        validate(&self_cycle),
        Err(IrDocumentValidationError::ExtractionCycle(_))
    ));
}

#[test]
fn extraction_shared_origins_are_valid_and_existing_mapping_limits_apply() {
    let mut document = extraction_document();
    let mut file = document.files[0].clone();
    file.id = FileId::from_bytes([240; 20]);
    file.path = "second.source".to_owned();
    file.path_locator = None;
    let source = source_for(&document, file.id, file.content_hash, file.byte_length);
    file.evidence.source = Some(source.clone());
    document.files.push(file);
    let mut second = document.source_mappings[0].clone();
    second.id = FactId::from_bytes([245; 20]);
    second.from = source;
    document.source_mappings.push(second);
    validate(&document).unwrap();
    let limits = IrLimits {
        max_source_mappings: 1,
        ..IrLimits::default()
    };
    assert!(validate_ir_document(&document, &limits, &ExtensionSupport::default()).is_err());
}

#[test]
fn extraction_nested_containers_validate_iteratively_and_detect_a_deep_cycle() {
    let mut document = extraction_document();
    let first = document.source_mappings[0].from.clone();
    let mut previous = document.source_mappings[0].to.clone();
    for index in 0_u64..1024 {
        let mut file = document.files[1].clone();
        let mut identity = [199; 20];
        identity[..8].copy_from_slice(&index.to_le_bytes());
        file.id = FileId::from_bytes(identity);
        file.path = format!("nested/{index}.container");
        let source = source_for(&document, file.id, file.content_hash, file.byte_length);
        file.evidence.source = Some(source.clone());
        document.files.push(file);
        let mut mapping = document.source_mappings[0].clone();
        mapping.id = rootlight_ids::derive_fact("test.extraction", &index.to_le_bytes()).id();
        mapping.from = previous;
        mapping.to = source.clone();
        document.source_mappings.push(mapping);
        previous = source;
    }
    validate(&document).unwrap();
    document.source_mappings.last_mut().unwrap().to = first;
    assert!(matches!(
        validate(&document),
        Err(IrDocumentValidationError::ExtractionCycle(_))
    ));
}
