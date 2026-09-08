//! Mixed-version composition contracts for independently validated IR partitions.
//! Frozen common facts exercise merging, not language-specific parser semantics.

use super::*;
use rootlight_ir::{IrDocument, NormalizedIrVersion, decode_ir_document};

fn fixture(version: NormalizedIrVersion) -> NormalizedIrDocument {
    let IrDocument::NormalizedV1_1(mut document) = decode_ir_document(
        include_bytes!("../../../tests/fixtures/compatibility/ir/1.1/document.json"),
        &IrLimits::default(),
        &ExtensionSupport::default(),
    )
    .expect("frozen common facts decode") else {
        panic!("fixture is normalized IR 1.1");
    };
    document.version = version;
    document
}

#[test]
fn mixed_version_appends_preserve_every_fact_and_required_version() {
    for target_version in [
        NormalizedIrVersion::V1_1,
        NormalizedIrVersion::V1_2,
        NormalizedIrVersion::V1_3,
        NormalizedIrVersion::V1_4,
    ] {
        for source_version in [
            NormalizedIrVersion::V1_1,
            NormalizedIrVersion::V1_2,
            NormalizedIrVersion::V1_3,
            NormalizedIrVersion::V1_4,
        ] {
            for path in ["structural", "project", "partition"] {
                let source = fixture(source_version);
                let mut target = NormalizedIrDocument::empty(source.repository, source.generation);
                target.version = target_version;
                let mut expected = source.clone();
                expected.version = target_version.max(source_version);
                let limits = IrLimits::default();
                let mut state = DocumentAppendState::from_document(&target).expect("append state");
                match path {
                    "structural" => {
                        append_normalized_document(&mut target, source, &limits, &mut state)
                    }
                    "project" => append_project_document_with_capacity(
                        &mut target,
                        source,
                        &limits,
                        &mut state,
                    ),
                    "partition" => merge_project_document(
                        &mut target,
                        source,
                        &mut BTreeSet::new(),
                        limits.max_diagnostics,
                        &limits,
                    )
                    .map(|truncation| {
                        assert!(!truncation.facts);
                        assert!(!truncation.diagnostics);
                    }),
                    _ => unreachable!("closed fixture paths"),
                }
                .unwrap_or_else(|error| {
                    panic!("{path} {target_version:?} + {source_version:?}: {error:?}")
                });
                assert_eq!(target, expected, "{path}");
                rootlight_ir::validate_ir_document(&target, &limits, &ExtensionSupport::default())
                    .expect("merged facts retain their contract");
            }
        }
    }
}

#[test]
fn mixed_version_supplemental_relations_preserve_bound_evidence() {
    for (left, right) in [
        (NormalizedIrVersion::V1_1, NormalizedIrVersion::V1_2),
        (NormalizedIrVersion::V1_2, NormalizedIrVersion::V1_1),
    ] {
        let source = fixture(right);
        let mut target = fixture(left);
        target.relations.clear();
        let actual =
            supplemental_project_relations(&target, source.clone(), &[], &Cancellation::new())
                .expect("common relations are compatible across supported minor versions");
        assert_eq!(actual, source.relations);
    }
}

#[test]
fn mixed_version_refinement_retains_occurrences_and_their_version() {
    for (left, right) in [
        (NormalizedIrVersion::V1_1, NormalizedIrVersion::V1_2),
        (NormalizedIrVersion::V1_2, NormalizedIrVersion::V1_1),
    ] {
        let source = fixture(right);
        let mut target = fixture(left);
        target.occurrences.clear();
        let actual = refinement::retain_structural_occurrences(
            target,
            std::slice::from_ref(&source),
            &IrLimits::default(),
            &Cancellation::new(),
        )
        .expect("refinement accepts compatible supported versions");
        assert_eq!(actual.version, NormalizedIrVersion::V1_2);
        assert_eq!(actual.occurrences, source.occurrences);
        assert_eq!(actual.entities, source.entities);
        assert_eq!(actual.files, source.files);
        rootlight_ir::validate_ir_document(
            &actual,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("refinement retains a valid source evidence graph");
    }
}

#[test]
fn rejected_mixed_version_append_does_not_promote_or_mutate_the_target() {
    for failure in [
        "repository",
        "generation",
        "capacity",
        "style_rule",
        "keyframes",
    ] {
        for path in ["structural", "project", "partition"] {
            let mut source = fixture(NormalizedIrVersion::V1_2);
            let mut target = fixture(NormalizedIrVersion::V1_1);
            let mut limits = IrLimits::default();
            match failure {
                "repository" => {
                    source.repository = derive_repository(b"different-merge-repository").id()
                }
                "generation" => source.generation = GenerationId::from_bytes([82; 20]),
                "capacity" => limits.max_files = 1,
                "style_rule" | "keyframes" => {
                    target.version = NormalizedIrVersion::V1_2;
                    source.version = NormalizedIrVersion::V1_1;
                    source.entities[0].kind = if failure == "style_rule" {
                        EntityKind::StyleRule
                    } else {
                        EntityKind::Keyframes
                    };
                }
                _ => unreachable!("closed failure cases"),
            }
            let expected = target.clone();
            let mut state = DocumentAppendState::from_document(&target).expect("append state");
            let expected_extension_bytes = state.extension_payload_bytes;
            let result = match path {
                "structural" => {
                    append_normalized_document(&mut target, source, &limits, &mut state)
                }
                "project" => {
                    append_project_document_with_capacity(&mut target, source, &limits, &mut state)
                }
                "partition" => merge_project_document(
                    &mut target,
                    source,
                    &mut BTreeSet::new(),
                    limits.max_diagnostics,
                    &limits,
                )
                .map(|_| ()),
                _ => unreachable!("closed fixture paths"),
            };
            assert!(result.is_err(), "{path} must reject {failure}");
            assert_eq!(target, expected, "{path} {failure}");
            assert_eq!(state.extension_payload_bytes, expected_extension_bytes);
            assert_eq!(state.truncated_diagnostics, 0);
            assert_eq!(state.truncated_extensions, 0);
            assert_eq!(state.truncated_skipped_regions, 0);
        }
    }
}

#[test]
fn promotion_does_not_legalize_an_invalid_baseline_target() {
    for kind in [EntityKind::StyleRule, EntityKind::Keyframes] {
        let mut target = fixture(NormalizedIrVersion::V1_1);
        target.entities[0].kind = kind;
        let mut source = NormalizedIrDocument::empty(target.repository, target.generation);
        source.version = NormalizedIrVersion::V1_2;
        let expected = target.clone();
        let mut state = DocumentAppendState::from_document(&target).expect("append state");
        assert_eq!(
            append_normalized_document(
                &mut target,
                source.clone(),
                &IrLimits::default(),
                &mut state
            ),
            Err(FirstSliceError::Identity),
        );
        assert_eq!(target, expected);
        assert_eq!(
            supplemental_project_relations(&target, source.clone(), &[], &Cancellation::new()),
            Err(FirstSliceError::Identity),
        );
        assert_eq!(
            refinement::retain_structural_occurrences(
                target,
                &[source],
                &IrLimits::default(),
                &Cancellation::new()
            ),
            Err(FirstSliceError::Identity),
        );
    }
}

#[test]
fn entity_promotion_rejects_invalid_lower_version_entities_without_mutation() {
    for lower in [
        NormalizedIrVersion::V1_1,
        NormalizedIrVersion::V1_2,
        NormalizedIrVersion::V1_3,
    ] {
        for kind in [
            EntityKind::MarkupElement,
            EntityKind::MarkupAttribute,
            EntityKind::Event,
            EntityKind::ErrorDeclaration,
            EntityKind::Modifier,
        ] {
            if lower >= kind.minimum_ir_version() {
                continue;
            }
            for invalid_target in [false, true] {
                let mut invalid = fixture(lower);
                invalid.entities[0].kind = kind;
                let mut valid = NormalizedIrDocument::empty(invalid.repository, invalid.generation);
                valid.version = kind.minimum_ir_version();
                let (mut target, source) = if invalid_target {
                    (invalid, valid)
                } else {
                    (valid, invalid)
                };
                let before = target.clone();
                let mut state = DocumentAppendState::from_document(&target).unwrap();
                assert_eq!(
                    append_normalized_document(
                        &mut target,
                        source,
                        &IrLimits::default(),
                        &mut state
                    ),
                    Err(FirstSliceError::Identity)
                );
                assert_eq!(target, before);
            }
        }
    }
}
