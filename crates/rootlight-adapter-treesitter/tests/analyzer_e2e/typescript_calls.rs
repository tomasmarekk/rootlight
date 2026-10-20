//! Direct module function calls require native binding and write-absence proof.
//! Public regressions retain the authored call leaf through normalized identity.

use super::*;
use rootlight_ir::{FactRef, derive_relation_record_id};

const VUE: &str = include_str!("../fixtures/typescript_calls/vue-util.ts");

fn case() -> LanguageCase {
    *CASES.iter().find(|case| case.name == "typescript").unwrap()
}

fn run(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let case = case();
    run_with(&analyzer(&provider, case), source)
}

fn run_with(analyzer: &TreeSitterAnalyzer, source: &str) -> AnalysisOutput {
    let case = case();
    let fixture = Fixture::new(case, source.as_bytes());
    let result = analyze(
        analyzer,
        &request(&fixture.snapshot, &fixture.source, case, &limits()),
        &ExtensionSupport::default(),
    );
    validate_ir_document(
        result.document(),
        limits().ir(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    result
}

fn check_call(output: &AnalysisOutput, source: &str, owner: &str, target: &str, start: usize) {
    let document = output.document();
    let function = |name: &str| {
        let matches = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "typescript"
                    && entity.kind == EntityKind::Function
                    && entity.canonical_name == name
            })
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "function {name}");
        matches[0]
    };
    let owner = function(owner);
    let target = function(target);
    let calls = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Calls
                && relation.subject == RelationEndpoint::Entity(owner.id)
                && relation.object == RelationEndpoint::Entity(target.id)
                && relation.evidence.source.as_ref().is_some_and(|reference| {
                    reference.span().start_byte() == u64::try_from(start).unwrap()
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls.len(),
        1,
        "exact source-backed direct call {owner:?} -> {target:?}"
    );
    let relation = calls[0];
    let witness = relation.evidence.source.as_ref().unwrap();
    assert_eq!(
        witness.span().end_byte(),
        u64::try_from(start + target.canonical_name.len()).unwrap()
    );
    assert_eq!(
        &source.as_bytes()[start..usize::try_from(witness.span().end_byte()).unwrap()],
        target.canonical_name.as_bytes()
    );
    assert_eq!(witness.content_hash(), content_hash(source.as_bytes()));
    let occurrence = document
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.enclosing == Some(owner.id)
                && occurrence.target == (OccurrenceTarget::Resolved { symbol: target.id })
                && occurrence.source == *witness
        })
        .unwrap();
    let mut parents = vec![
        FactRef::Entity(owner.id),
        FactRef::Fact(occurrence.id),
        FactRef::Entity(target.id),
    ];
    parents.sort_unstable();
    parents.dedup();
    assert_eq!(relation.evidence.derivation, parents);
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
}

#[test]
fn vue_direct_module_function_call_has_exact_written_binding() {
    assert_eq!(&VUE[2018..2023], "isDef");
    let output = run(VUE);
    check_call(&output, VUE, "isPromise", "isDef", 2018);
}

#[test]
fn value_bindings_writes_and_dynamic_contexts_never_invent_exact_calls() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case());
    for body in [
        "export function caller(target: () => unknown){return target()}",
        "export function caller(){let target=()=>0;return target()}",
        "export function caller(){target();var target: () => unknown}",
        "export function caller(){ {const target=()=>0;target()} }",
        "export function caller(){try{throw 0}catch(target){target()}}",
        "export function caller(){const {target}=holder;target()}",
        "export function caller(){const [target]=holder;target()}",
        "import {other as target} from './other';export function caller(){target()}",
        "target=replacement;export function caller(){target()}",
        "function mutate(){target=replacement} export function caller(){target()}",
        "target+=replacement;export function caller(){target()}",
        "target||=replacement;export function caller(){target()}",
        "target++;export function caller(){target()}",
        "({target}=holder);export function caller(){target()}",
        "for(target of values){} export function caller(){target()}",
        "export function caller(){eval('target=replacement');target()}",
        r"export function caller(){e\u0076al('target=replacement');target()}",
        "export function caller(){return object.target()}",
        "export function caller(){return object['target']()}",
        "export function caller(){return target?.()}",
        "export function caller(){return (target)()}",
        "namespace nested.target {export const value=0} export function caller(){target()}",
        "export function caller(){with(scope){target()}}",
        "function target(value:unknown){return value} export function caller(){target()}",
    ] {
        let source = format!("export function target(value:unknown){{return value}}\n{body}");
        let output = run_with(&analyzer, &source);
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls),
            "{body}: {:?}",
            output.document().relations
        );
    }
    let script = "function target(){} function caller(){target()}";
    let output = run_with(&analyzer, script);
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn escaped_definition_and_nested_block_keep_exact_lexical_call_identity() {
    let source = r"export function t\u0061rget(value:unknown){return value} export function caller(){if(flag){return target(1)}}";
    let output = run(source);
    check_call(
        &output,
        source,
        "caller",
        "target",
        source.rfind("target(1)").unwrap(),
    );
}

#[test]
fn native_module_call_proof_replays_only_with_matching_source_generation() {
    let source = "export function target(value:unknown){return value} export function caller(){return target(1)}";
    let provider = Arc::new(provider());
    let case = case();
    let analyzer = analyzer(&provider, case);
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, case, &budget);
    let fresh = analyze(&analyzer, &next_request, &ExtensionSupport::default());
    let replay = analyzer
        .analyze_from_artifact(
            &next_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    check_call(
        &first,
        source,
        "caller",
        "target",
        source.rfind("target(1)").unwrap(),
    );
    check_call(
        &replay,
        source,
        "caller",
        "target",
        source.rfind("target(1)").unwrap(),
    );
    assert!(
        replay
            .document()
            .occurrences
            .iter()
            .all(|row| row.source.generation() == next.source.generation())
    );
    let changed = fixture.rewrite(
        source
            .replace("return target(1)", "target=replacement;return target(1)")
            .as_bytes(),
    );
    let changed_request = request(&changed.snapshot, &changed.source, case, &budget);
    assert!(
        analyzer
            .analyze_from_artifact(
                &changed_request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
}
