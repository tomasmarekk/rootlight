//! Recovery is a separate source-bound explanation, not a source payload mode.

use super::*;
use rootlight_ids::{ContentHash, GenerationId, RepositoryId};
use rootlight_ir::{AggregateCapacityResource, AggregateCapacityStage, SourceRef, SourceSpan};
use serde_json::{Value, json};

fn chunk(file: u8) -> SourceChunkV1_3 {
    let hash = ContentHash::from_bytes([4; 32]);
    SourceChunkV1_3::Legacy(crate::vertical::SourceChunk {
        extracted_from: None,
        source_ref: SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(FileId::from_bytes([file; 20]), 0, 3).unwrap(),
            hash,
            None,
        ),
        path: format!("source-{file}.graphql"),
        start_byte: 0,
        end_byte: 3,
        start_line: None,
        end_line: None,
        content: "abc".into(),
        encoding: crate::vertical::SourceEncoding::Utf8,
        content_hash: hash,
        language: "graphql".into(),
        generated: false,
        trust: crate::TrustClassification::UntrustedRepositoryData,
    })
}

fn recovery() -> SourceReadRecovery {
    SourceReadRecovery {
        cause: AggregateSourceRecovery {
            source_inventory: ContentHash::from_bytes([9; 32]),
            members: 10,
            derived_members: 1,
            stage: AggregateCapacityStage::StructuralAppend,
            resource: AggregateCapacityResource::Entities,
            observed: 11,
            limit: 10,
            text_snapshot_bytes: 8192,
        },
        files: vec![3, 4]
            .into_iter()
            .map(|file| SourceRecoveryFile {
                file: FileId::from_bytes([file; 20]),
                text_search: SourceTextSearchCoverage::WholeSourceProjection,
            })
            .collect(),
    }
}

fn data() -> SourceReadDataV1_4 {
    SourceReadDataV1_4 {
        chunks: vec![chunk(3), chunk(4), chunk(3)],
        stale_references: Vec::new(),
        elisions: Vec::new(),
        total_source_bytes: 9,
        total_text_bytes: 9,
        explanation: None,
        conditional_activity: Vec::new(),
        source_recovery: Some(recovery()),
    }
}

#[test]
fn aggregate_recovery_keeps_exact_chunks_and_all_search_dispositions() {
    for disposition in [
        SourceTextSearchCoverage::WholeSourceProjection,
        SourceTextSearchCoverage::MetadataOnlyCaptured,
        SourceTextSearchCoverage::UnsupportedEncoding,
    ] {
        let mut value = data();
        value.source_recovery.as_mut().unwrap().files[1].text_search = disposition;
        let wire = serde_json::to_value(&value).unwrap();
        let read: SourceReadDataV1_4 = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(read, value);
        assert_eq!(wire["chunks"][0]["content"], "abc");
        assert_eq!(
            wire["chunks"][0]["source_ref"],
            serde_json::to_value(value.chunks[0].source_ref()).unwrap()
        );
        assert_eq!(
            wire["source_recovery"]["files"].as_array().unwrap().len(),
            2
        );
        assert!(wire.get("entities").is_none());
    }
}

#[test]
fn aggregate_recovery_rejects_duplicate_unsorted_missing_and_foreign_rows() {
    for mutation in 0..5 {
        let mut value = data();
        let files = &mut value.source_recovery.as_mut().unwrap().files;
        match mutation {
            0 => files.push(files[0].clone()),
            1 => files.reverse(),
            2 => {
                files.pop();
            }
            3 => files[1].file = FileId::from_bytes([5; 20]),
            _ => files.clear(),
        }
        assert!(
            serde_json::from_value::<SourceReadDataV1_4>(serde_json::to_value(value).unwrap())
                .is_err()
        );
    }
}

#[test]
fn aggregate_recovery_rejects_impossible_or_incompatible_capacity_causes() {
    for mutation in 0..7 {
        let mut value = data();
        let cause = &mut value.source_recovery.as_mut().unwrap().cause;
        match mutation {
            0 => cause.members = 0,
            1 => cause.derived_members = cause.members,
            2 => cause.observed = cause.limit,
            3 => cause.observed = u64::MAX,
            4 => cause.text_snapshot_bytes = 0,
            5 => cause.stage = AggregateCapacityStage::GenerationText,
            _ => {
                cause.members = 1;
                cause.derived_members = 0;
            }
        }
        assert!(
            serde_json::from_value::<SourceReadDataV1_4>(serde_json::to_value(value).unwrap())
                .is_err()
        );
    }
}

#[test]
fn aggregate_recovery_requires_one_repository_and_generation_context() {
    for foreign_repository in [false, true] {
        let mut value = data();
        let SourceChunkV1_3::Legacy(chunk) = &mut value.chunks[1] else {
            panic!("legacy test chunk");
        };
        let original = &chunk.source_ref;
        chunk.source_ref = SourceRef::new(
            if foreign_repository {
                RepositoryId::from_bytes([8; 16])
            } else {
                original.repository()
            },
            if foreign_repository {
                original.generation()
            } else {
                GenerationId::from_bytes([8; 20])
            },
            original.span(),
            original.content_hash(),
            None,
        );
        assert!(value.validate().is_err());
    }
}

#[test]
fn aggregate_recovery_is_new_vocabulary_and_absence_has_no_support_claim() {
    let wire = serde_json::to_value(data()).unwrap();
    assert!(serde_json::from_value::<crate::source_text::SourceReadDataV1_3>(wire).is_err());
    let mut value = data();
    value.source_recovery = None;
    let wire = serde_json::to_value(value).unwrap();
    assert!(wire.get("source_recovery").is_none());
    assert!(serde_json::from_value::<crate::source_text::SourceReadDataV1_3>(wire.clone()).is_ok());
    assert!(serde_json::from_value::<SourceReadDataV1_4>(wire).is_ok());
}

#[test]
fn aggregate_recovery_preserves_text_total_and_closed_field_validation() {
    let wire = serde_json::to_value(data()).unwrap();
    for pointer in ["/total_source_bytes", "/total_text_bytes"] {
        let mut invalid = wire.clone();
        *invalid.pointer_mut(pointer).unwrap() = json!(8);
        assert!(serde_json::from_value::<SourceReadDataV1_4>(invalid).is_err());
    }
    let mut invalid = wire;
    invalid["source_recovery"]["files"][0]["full_language_support"] = Value::Bool(true);
    assert!(serde_json::from_value::<SourceReadDataV1_4>(invalid).is_err());
}
