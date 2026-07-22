//! Deterministic adversarial coverage for the public advanced-query grammar.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use proptest::prelude::*;
use proptest::test_runner::{RngAlgorithm, RngSeed};
use rootlight_ids::SymbolId;
use rootlight_query::{
    ADVANCED_MAX_DEPTH, ADVANCED_MAX_TRAVERSAL, AdvancedAggregateFunction, AdvancedAstNode,
    AdvancedEntityKind, AdvancedPredicate, AdvancedQueryPlan, AdvancedRelationKind,
    AdvancedSortKey, AdvancedTraverseDirection, AdvancedValue,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const CAMPAIGN_SEED: u64 = 202_607_220_040;
const CI_CASES: u32 = 96;
const MAX_GATE_CASES: u32 = 4_096;
const MAX_PAYLOAD_BYTES: usize = 4_096;
const CORPUS_CASE_IDS: [&str; 12] = [
    "raw-sql",
    "unknown-operator",
    "raw-cypher",
    "shell-command",
    "unrestricted-regex",
    "unknown-function",
    "field-object-injection",
    "operator-parameter-injection",
    "malformed-json",
    "excessive-traversal-depth",
    "cursor-malformed",
    "cursor-truncated",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    schema_version: String,
    campaign: Campaign,
    cases: Vec<CorpusCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Campaign {
    algorithm: String,
    seed: u64,
    ci_cases: u32,
    gate_cases_environment: String,
    gate_cases_maximum: u32,
    maximum_payload_bytes: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusCase {
    id: String,
    boundary: String,
    payload: String,
    sha256: String,
}

fn corpus_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/adversarial/query-advanced/v1/corpus.json")
}

fn corpus() -> Corpus {
    let bytes = fs::read(corpus_path()).expect("advanced adversarial corpus is readable");
    serde_json::from_slice(&bytes).expect("advanced adversarial corpus is valid")
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn campaign_cases() -> u32 {
    std::env::var("ROOTLIGHT_ADVANCED_GATE_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|cases| (1..=MAX_GATE_CASES).contains(cases))
        .unwrap_or(CI_CASES)
}

fn field_name() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["id", "name", "kind", "path"]).prop_map(str::to_owned)
}

fn scalar() -> impl Strategy<Value = AdvancedValue> {
    prop_oneof![
        "[a-zA-Z0-9_./-]{0,32}".prop_map(AdvancedValue::Text),
        any::<i64>().prop_map(AdvancedValue::Integer),
        any::<bool>().prop_map(AdvancedValue::Boolean),
        any::<[u8; 20]>().prop_map(|bytes| AdvancedValue::Symbol(SymbolId::from_bytes(bytes))),
    ]
}

fn predicate() -> impl Strategy<Value = AdvancedPredicate> {
    let leaf = prop_oneof![
        (field_name(), scalar())
            .prop_map(|(field, value)| AdvancedPredicate::Equals { field, value }),
        (field_name(), scalar())
            .prop_map(|(field, value)| AdvancedPredicate::NotEquals { field, value }),
        (field_name(), prop::collection::vec(scalar(), 1..=4),)
            .prop_map(|(field, values)| AdvancedPredicate::In { field, values }),
    ];
    leaf.prop_recursive(2, 15, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 1..=3)
                .prop_map(|predicates| AdvancedPredicate::And { predicates }),
            prop::collection::vec(inner, 1..=3)
                .prop_map(|predicates| AdvancedPredicate::Or { predicates }),
        ]
    })
}

fn entity_kind() -> impl Strategy<Value = AdvancedEntityKind> {
    prop::sample::select(vec![
        AdvancedEntityKind::File,
        AdvancedEntityKind::Module,
        AdvancedEntityKind::Type,
        AdvancedEntityKind::Function,
        AdvancedEntityKind::Method,
        AdvancedEntityKind::Field,
        AdvancedEntityKind::Constant,
        AdvancedEntityKind::Variable,
        AdvancedEntityKind::Configuration,
    ])
}

fn relation_kind() -> impl Strategy<Value = AdvancedRelationKind> {
    prop::sample::select(vec![
        AdvancedRelationKind::Calls,
        AdvancedRelationKind::CalledBy,
        AdvancedRelationKind::Imports,
        AdvancedRelationKind::ImportedBy,
        AdvancedRelationKind::Tests,
        AdvancedRelationKind::TestedBy,
        AdvancedRelationKind::Contains,
        AdvancedRelationKind::ContainedBy,
        AdvancedRelationKind::Implements,
        AdvancedRelationKind::ImplementedBy,
        AdvancedRelationKind::References,
        AdvancedRelationKind::ReferencedBy,
    ])
}

fn valid_ast() -> impl Strategy<Value = AdvancedAstNode> {
    let leaf = prop_oneof![
        (entity_kind(), proptest::option::of(predicate())).prop_map(|(entity, filter)| {
            AdvancedAstNode::Scan {
                entity,
                filter: filter.map(Box::new),
            }
        }),
        (
            any::<[u8; 20]>(),
            relation_kind(),
            prop::sample::select(vec![
                AdvancedTraverseDirection::Inbound,
                AdvancedTraverseDirection::Outbound,
                AdvancedTraverseDirection::Both,
            ]),
            1_u8..=5,
        )
            .prop_map(
                |(seed, relation, direction, max_depth)| AdvancedAstNode::Traverse {
                    seed: Some(SymbolId::from_bytes(seed)),
                    seed_from: None,
                    relation,
                    direction,
                    max_depth: Some(max_depth),
                }
            ),
    ];

    leaf.prop_recursive(4, 31, 2, |inner| {
        prop_oneof![
            (inner.clone(), predicate()).prop_map(|(input, predicate)| AdvancedAstNode::Filter {
                input: Box::new(input),
                predicate,
            }),
            (inner.clone(), prop::collection::vec(field_name(), 1..=4),).prop_map(
                |(input, columns)| AdvancedAstNode::Project {
                    input: Box::new(input),
                    columns,
                }
            ),
            (inner.clone(), inner.clone(), field_name()).prop_map(|(left, right, on)| {
                AdvancedAstNode::Join {
                    left: Box::new(left),
                    right: Box::new(right),
                    on,
                }
            }),
            (inner.clone(), prop::collection::vec(field_name(), 0..=2)).prop_map(
                |(input, group_by)| AdvancedAstNode::Aggregate {
                    input: Box::new(input),
                    group_by,
                    aggregations: vec![AdvancedAggregateFunction::Count],
                },
            ),
            (inner.clone(), field_name(), any::<bool>()).prop_map(|(input, field, descending)| {
                AdvancedAstNode::Sort {
                    input: Box::new(input),
                    by: vec![AdvancedSortKey { field, descending }],
                }
            },),
            (inner, 1_u16..=1_000).prop_map(|(input, max_rows)| AdvancedAstNode::Limit {
                input: Box::new(input),
                max_rows,
            }),
        ]
    })
}

#[test]
fn minimized_corpus_has_fixed_order_integrity_and_bounds() {
    let corpus = corpus();
    assert_eq!(corpus.schema_version, "1.0");
    assert_eq!(corpus.campaign.algorithm, "chacha");
    assert_eq!(corpus.campaign.seed, CAMPAIGN_SEED);
    assert_eq!(corpus.campaign.ci_cases, CI_CASES);
    assert_eq!(
        corpus.campaign.gate_cases_environment,
        "ROOTLIGHT_ADVANCED_GATE_CASES"
    );
    assert_eq!(corpus.campaign.gate_cases_maximum, MAX_GATE_CASES);
    assert_eq!(corpus.campaign.maximum_payload_bytes, MAX_PAYLOAD_BYTES);

    let ids: Vec<&str> = corpus.cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids, CORPUS_CASE_IDS);
    assert_eq!(
        ids.iter().copied().collect::<BTreeSet<_>>().len(),
        ids.len()
    );
    for case in corpus.cases {
        assert!(case.payload.len() <= MAX_PAYLOAD_BYTES, "{}", case.id);
        assert_eq!(
            sha256_hex(case.payload.as_bytes()),
            case.sha256,
            "{}",
            case.id
        );
    }
}

#[test]
fn minimized_ast_injections_are_rejected_by_the_typed_grammar() {
    for case in corpus()
        .cases
        .into_iter()
        .filter(|case| case.boundary == "ast_deserialize")
    {
        assert!(
            serde_json::from_str::<AdvancedAstNode>(&case.payload).is_err(),
            "{} escaped the typed grammar",
            case.id
        );
    }
}

#[test]
fn corpus_keeps_planner_and_cursor_cases_separate_from_ast_rejections() {
    let corpus = corpus();
    let traversal = corpus
        .cases
        .iter()
        .find(|case| case.id == "excessive-traversal-depth")
        .expect("traversal-depth case exists");
    let ast: AdvancedAstNode =
        serde_json::from_str(&traversal.payload).expect("planner case has a typed AST");
    assert!(matches!(
        ast,
        AdvancedAstNode::Traverse {
            max_depth: Some(depth),
            ..
        } if usize::from(depth) > ADVANCED_MAX_DEPTH
    ));

    for id in ["cursor-malformed", "cursor-truncated"] {
        let case = corpus
            .cases
            .iter()
            .find(|case| case.id == id)
            .expect("cursor case exists");
        assert_eq!(case.boundary, "cursor_decode");
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: campaign_cases(),
        max_shrink_iters: 512,
        failure_persistence: None,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(CAMPAIGN_SEED),
        ..ProptestConfig::default()
    })]

    #[test]
    fn valid_asts_round_trip_to_the_same_bounded_plan(
        ast in valid_ast(),
        max_rows in 1_usize..=1_000,
        max_traversal in 1_usize..=ADVANCED_MAX_TRAVERSAL,
    ) {
        let encoded = serde_json::to_vec(&ast).expect("generated AST serializes");
        let decoded: AdvancedAstNode =
            serde_json::from_slice(&encoded).expect("generated AST deserializes");
        let (operators, depth) = ast.derive_plan_shape();
        let (decoded_operators, decoded_depth) = decoded.derive_plan_shape();
        let plan = AdvancedQueryPlan::validate(
            &operators,
            max_rows,
            max_traversal,
            depth,
        );
        let replay = AdvancedQueryPlan::validate(
            &decoded_operators,
            max_rows,
            max_traversal,
            decoded_depth,
        );

        prop_assert_eq!(decoded, ast);
        prop_assert_eq!(decoded_operators, operators);
        prop_assert_eq!(decoded_depth, depth);
        prop_assert_eq!(
            replay.map_err(|error| error.to_string()),
            plan.map_err(|error| error.to_string()),
        );
        prop_assert!(encoded.len() <= 32 * 1024);
    }

    #[test]
    fn bounded_deserializer_replay_never_escapes_the_typed_plan(
        bytes in prop::collection::vec(any::<u8>(), 0..=MAX_PAYLOAD_BYTES),
    ) {
        if let Ok(ast) = serde_json::from_slice::<AdvancedAstNode>(&bytes) {
            let (operators, depth) = ast.derive_plan_shape();
            let first = AdvancedQueryPlan::validate(
                &operators,
                100,
                ADVANCED_MAX_TRAVERSAL,
                depth,
            );
            let replay = AdvancedQueryPlan::validate(
                &operators,
                100,
                ADVANCED_MAX_TRAVERSAL,
                depth,
            );
            prop_assert_eq!(
                replay.map_err(|error| error.to_string()),
                first.map_err(|error| error.to_string()),
            );
        }
    }
}
