//! Schema type edges reserve their complete upper bound before IR materialization.
//! Real parser facts exercise record and derivation quotas independently of the
//! final sink, which would otherwise detect under-accounting only after lowering.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{BatchThresholds, GenerationBoundSnapshot, LanguageId, StreamLimits};
use rootlight_ids::{GenerationId, derive_repository};
use rootlight_ir::BuildContextIdentity;
use rootlight_vfs::{RelativePath, RepositoryRoot};

use super::*;

const CASES: [(&str, &str, &str); 3] = [
    (
        "thrift",
        "schema.thrift",
        "struct Item {} struct Host { 1: Item a; 2: Item b; 3: Item c; }\n",
    ),
    (
        "capnp",
        "schema.capnp",
        "struct Item {} struct Host { a @0 :Item; b @1 :Item; c @2 :Item; }\n",
    ),
    (
        "graphql",
        "schema.graphql",
        "type Item { self: Item } type Host { a: Item b: Item }\n",
    ),
];

#[derive(Clone, Copy)]
enum Boundary {
    Unbounded,
    Relations,
    TotalDerivations,
    PerRecordDerivations,
}

fn limits(ir: IrLimits) -> AnalysisLimits {
    let stream = StreamLimits::new(
        128,
        16_384,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).unwrap(),
    )
    .unwrap();
    AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        32,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        ir,
    )
    .unwrap()
}

fn preflight(
    language: &str,
    path: &str,
    source: &str,
    boundary: Boundary,
) -> (Result<LoweringBudget, AdapterError>, usize) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join(path), source).unwrap();
    let repository = derive_repository(b"schema-field-capacity").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 1024 * 1024)
        .unwrap();
    let source_ref = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(source.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = crate::TreeSitterProvider::new(
        crate::RuntimeConfig::new(
            1024 * 1024,
            16_384,
            128,
            32,
            64,
            1,
            16 * 1024 * 1024,
            crate::ParserSettings::new(4096).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let producer =
        ProducerIdentity::new("schema-field-capacity", "1.0", content_hash(b"test")).unwrap();
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(provider),
        producer.clone(),
        LanguageId::new(language).unwrap(),
        "native-schema-capacity",
        content_hash(b"schema-capacity-binary"),
    )
    .unwrap();
    let parse_limits = limits(IrLimits::default());
    let parse_request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
        LanguageId::new(language).unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"schema-capacity-build")),
        &parse_limits,
    )
    .unwrap()
    .with_generated_status(false);
    let cancellation = Cancellation::new();
    let parsed = analyzer
        .parse_request(&parse_request, &cancellation)
        .unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(|fact| occurrence_role(fact) == Some(OccurrenceRole::TypeUse))
            .count(),
        3,
        "{language} fixture has exactly three written type uses"
    );
    let entities = parsed
        .facts()
        .iter()
        .filter(|fact| structural_entity_kind(fact).is_some())
        .count();
    let mut ir = IrLimits::default();
    let expected = match boundary {
        Boundary::Unbounded | Boundary::TotalDerivations => 11,
        Boundary::Relations => entities + 6,
        Boundary::PerRecordDerivations => 3,
    };
    match boundary {
        Boundary::Unbounded => {}
        Boundary::Relations => ir.max_relations = expected - 1,
        Boundary::TotalDerivations => ir.max_total_nested_items = expected - 1,
        Boundary::PerRecordDerivations => ir.max_nested_items_per_record = expected - 1,
    }
    let lowering_limits = limits(ir);
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source_ref).unwrap(),
        LanguageId::new(language).unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"schema-capacity-build")),
        &lowering_limits,
    )
    .unwrap()
    .with_generated_status(false);
    (
        preflight_lowering_limits(
            &request,
            &parsed,
            &producer,
            "native-schema-capacity",
            &[],
            &cancellation,
        ),
        expected,
    )
}

#[test]
fn field_relation_preflight_reserves_both_edges_before_materialization() {
    for (language, path, source) in CASES {
        let (admitted, expected) = preflight(language, path, source, Boundary::Unbounded);
        assert_eq!(admitted.unwrap().nested_items, expected);
        let (rejected, expected) = preflight(language, path, source, Boundary::Relations);
        assert!(matches!(
            rejected,
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::Records,
                observed,
                limit,
            })) if observed == expected && limit == expected - 1
        ));
    }
}

#[test]
fn field_relation_preflight_reserves_all_derivations_before_materialization() {
    for (language, path, source) in CASES {
        for boundary in [Boundary::TotalDerivations, Boundary::PerRecordDerivations] {
            let (rejected, expected) = preflight(language, path, source, boundary);
            assert!(matches!(
                rejected,
                Err(AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::NestedItems,
                    observed,
                    limit,
                })) if observed == expected && limit == expected - 1
            ));
        }
    }
}
