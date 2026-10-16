//! Default aggregate admission preserves concrete per-file gap evidence.

use super::*;

#[test]
fn default_ir_gap_credit_survives_resolution() {
    let decoded = rootlight_ir::decode_ir_document(
        include_bytes!("../../../../tests/fixtures/compatibility/ir/1.1/document.json"),
        &IrLimits::default(),
        &rootlight_ir::ExtensionSupport::default(),
    )
    .unwrap();
    let rootlight_ir::IrDocument::NormalizedV1_1(mut document) = decoded else {
        panic!("the frozen fixture is normalized IR 1.1");
    };
    document
        .skipped_regions
        .resize(100_001, document.skipped_regions[0].clone());
    for (index, gap) in document.skipped_regions.iter_mut().enumerate() {
        gap.detail = format!("unavailable written construct {index}");
        gap.id = rootlight_ir::derive_skipped_region_id(gap).unwrap();
    }
    ResolutionEngine::new(resolution_limits_for_occurrences(document.occurrences.len()).unwrap())
        .estimate_work(&document, &Cancellation::new())
        .expect("resolution accepts the concrete gaps admitted during analysis");
    append_resolution_work_bounded_diagnostic(&mut document, 2, 1, &IrLimits::default())
        .expect("resolution reporting preserves already admitted concrete gap records");
    assert_eq!(document.skipped_regions.len(), 100_001);
    assert!(
        document
            .diagnostics
            .iter()
            .any(|record| record.code == "resolution-work-bounded")
    );
}

fn gap_document(
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    count: usize,
) -> NormalizedIrDocument {
    let bytes = vec![b'x'; count];
    let hash = content_hash(&bytes);
    let mut document = NormalizedIrDocument::empty(repository, generation);
    for offset in 0..count {
        let start = u64::try_from(offset).unwrap();
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, start, start + 1).unwrap(),
            hash,
            None,
        );
        let mut gap = rootlight_ir::SkippedRegion {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            source: source.clone(),
            domain: rootlight_ir::FactDomain::Entities,
            reason: rootlight_ir::SkippedRegionReason::UnsupportedConstruct,
            detail: "unavailable written construct".to_owned(),
            provenance: FactId::from_bytes([7; 20]),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        };
        gap.id = rootlight_ir::derive_skipped_region_id(&gap).unwrap();
        document.skipped_regions.push(gap);
    }
    document
}

#[test]
fn default_gap_credit_retains_every_region_across_files() {
    let repository = derive_repository(b"per-file-gap-retention").id();
    let generation = GenerationId::from_bytes([1; 20]);
    let limits = analysis_limits(64 * 1024 * 1024).unwrap();
    let mut target = NormalizedIrDocument::empty(repository, generation);
    let mut state = DocumentAppendState::from_document(&target).unwrap();
    let mut expected = Vec::new();
    for (file, count) in [
        (FileId::from_bytes([2; 20]), 100_001),
        (FileId::from_bytes([3; 20]), 3),
    ] {
        let source = gap_document(repository, generation, file, count);
        expected.extend(source.skipped_regions.iter().map(|gap| gap.id));
        append_normalized_document(&mut target, source, limits.ir(), &mut state).unwrap();
    }
    assert_eq!(target.skipped_regions.len(), 100_004);
    assert_eq!(state.truncated_skipped_regions, 0);
    assert_eq!(
        target
            .skipped_regions
            .iter()
            .map(|gap| gap.id)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(target.skipped_regions.iter().all(|gap| {
        gap.evidence.source.as_ref() == Some(&gap.source)
            && gap.domain == rootlight_ir::FactDomain::Entities
            && gap.reason == rootlight_ir::SkippedRegionReason::UnsupportedConstruct
            && gap.detail == "unavailable written construct"
    }));
    let public_defaults = IrLimits::default();
    assert_eq!(
        limits.ir().max_total_records,
        public_defaults.max_total_records
    );
    assert_eq!(
        limits.ir().max_total_string_bytes,
        public_defaults.max_total_string_bytes
    );
    assert_eq!(
        limits.ir().max_document_bytes,
        public_defaults.max_document_bytes
    );
}

#[test]
fn gap_credit_preserves_actual_string_quota_and_atomicity() {
    let repository = derive_repository(b"gap-string-quota").id();
    let generation = GenerationId::from_bytes([4; 20]);
    let source = gap_document(repository, generation, FileId::from_bytes([5; 20]), 2);
    let mut limits = analysis_limits(64 * 1024 * 1024).unwrap().ir().clone();
    limits.max_total_string_bytes = source.skipped_regions[0].detail.len();
    let mut target = NormalizedIrDocument::empty(repository, generation);
    let mut state = DocumentAppendState::from_document(&target).unwrap();
    let original_state = state.clone();
    assert!(matches!(
        append_normalized_document(&mut target, source, &limits, &mut state),
        Err(FirstSliceError::ResourceLimit {
            resource: FirstSliceResource::StringBytes,
            ..
        })
    ));
    assert!(target.skipped_regions.is_empty());
    assert_eq!(state, original_state);
}
