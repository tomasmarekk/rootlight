//! Public Clojure analysis preserves written bindings and direct lexical calls.
//! Reader extensions, evaluated data and dynamic binding remain explicit gaps.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId, GenerationBoundSnapshot,
    LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, FactRef,
    IrLimits, OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint,
    RelationPredicate, SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = include_str!("fixtures/clojure/kahn.cljs");

fn analyze(text: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    analyze_raw(text.as_bytes(), "utf-8")
}

fn analyze_raw(raw: &[u8], encoding: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.cljs"), raw).unwrap();
    let repository = derive_repository(b"clojure-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.cljs")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = TreeSitterProvider::new(
        RuntimeConfig::new(
            1024 * 1024,
            16_384,
            128,
            16,
            64,
            1,
            16 * 1024 * 1024,
            ParserSettings::new(256).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(provider),
        ProducerIdentity::new(
            "clojure-source-contract",
            "1.0",
            content_hash(b"clojure-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("clojure").unwrap(),
        "clojure-static-contract",
        content_hash(b"clojure-contract-binary"),
    )
    .unwrap();
    let batch = BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        4096,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        batch,
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("clojure").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"clojure-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let result = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )
    .unwrap();
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), content_hash(raw));
        assert_eq!(occurrence.source.repository(), repository);
        assert_eq!(occurrence.source.generation(), source.generation());
        assert!(occurrence.source.span().end_byte() <= u64::try_from(raw.len()).unwrap());
    }
    result
}

fn entity<'a>(
    output: &'a rootlight_adapter_sdk::AnalysisOutput,
    name: &str,
) -> &'a rootlight_ir::EntityRecord {
    let values = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name && entity.kind == EntityKind::Function)
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 1, "{name}: {:?}", output.document().entities);
    values[0]
}

fn calls(output: &rootlight_adapter_sdk::AnalysisOutput) -> Vec<&rootlight_ir::RelationRecord> {
    output
        .document()
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Calls)
        .collect()
}

fn assert_call(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    source: &str,
    from: &str,
    to: &str,
    marker: &str,
) {
    let owner = entity(output, from);
    let target = entity(output, to);
    let relations = calls(output)
        .into_iter()
        .filter(|relation| {
            relation.subject == RelationEndpoint::Entity(owner.id)
                && relation.object == RelationEndpoint::Entity(target.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        relations.len(),
        1,
        "{from} -> {to}: {:?}",
        output.document().relations
    );
    let relation = relations[0];
    let witness = relation.evidence.source.as_ref().unwrap();
    let start = source.find(marker).unwrap() + marker.find(to).unwrap();
    assert_eq!(witness.span().start_byte(), u64::try_from(start).unwrap());
    assert_eq!(
        witness.span().end_byte(),
        u64::try_from(start + to.len()).unwrap()
    );
    let occurrences = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.enclosing == Some(owner.id)
                && occurrence.target == OccurrenceTarget::Resolved { symbol: target.id }
                && occurrence.source == *witness
        })
        .collect::<Vec<_>>();
    assert_eq!(occurrences.len(), 1);
    let mut proof = vec![
        FactRef::Entity(owner.id),
        FactRef::Fact(occurrences[0].id),
        FactRef::Entity(target.id),
    ];
    proof.sort_unstable();
    proof.dedup();
    assert_eq!(relation.evidence.derivation, proof);
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
}

#[test]
fn lighttable_kahn_keeps_exact_namespace_functions_contains_and_vector_call() {
    assert_eq!(SOURCE.len(), 1520);
    let output = analyze(SOURCE);
    let namespace = output
        .document()
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Namespace && e.canonical_name == "lt.util.kahn")
        .unwrap();
    for (name, kind, start, end) in [
        ("lt.util.kahn", EntityKind::Namespace, 4, 16),
        ("without", EntityKind::Function, 196, 203),
        ("take-1", EntityKind::Function, 278, 284),
        ("no-incoming", EntityKind::Function, 461, 472),
    ] {
        let selected = output
            .document()
            .occurrences
            .iter()
            .filter(|o| {
                o.role == OccurrenceRole::Definition
                    && o.source.span().start_byte() == start
                    && o.source.span().end_byte() == end
            })
            .collect::<Vec<_>>();
        assert_eq!(
            selected.len(),
            1,
            "{name}: {:?}",
            output.document().entities
        );
        assert_eq!(
            selected[0].syntactic_text_hash,
            content_hash(name.as_bytes())
        );
        let OccurrenceTarget::Resolved { symbol } = selected[0].target else {
            panic!("definition must resolve")
        };
        let record = output
            .document()
            .entities
            .iter()
            .find(|e| e.id == symbol)
            .unwrap();
        assert_eq!(record.canonical_name, name);
        assert_eq!(record.kind, kind);
        if kind == EntityKind::Function {
            assert!(
                output
                    .document()
                    .relations
                    .iter()
                    .any(|r| r.predicate == RelationPredicate::Contains
                        && r.subject == RelationEndpoint::Entity(namespace.id)
                        && r.object == RelationEndpoint::Entity(symbol))
            );
        }
    }
    assert_call(&output, SOURCE, "take-1", "without", "(without s item)");
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::Definition && o.source.span().start_byte() == 435)
    );
    assert!(!calls(&output).iter().any(|r| {
        r.evidence
            .source
            .as_ref()
            .is_some_and(|s| s.span().start_byte() == 1422)
    }));
}

#[test]
fn evaluated_vector_map_set_and_let_bodies_keep_direct_calls() {
    let source = "(ns Sample) (defn target [x] x) (defn vector-call [x] [(target x)]) (defn map-call [x] {:v (target x)}) (defn set-call [x] #{(target x)}) (defn let-call [x] (let [item x] [item (target item)]))";
    let output = analyze(source);
    for from in ["vector-call", "map-call", "set-call", "let-call"] {
        let owner = entity(&output, from);
        let target = entity(&output, "target");
        assert!(
            calls(&output)
                .iter()
                .any(|r| r.subject == RelationEndpoint::Entity(owner.id)
                    && r.object == RelationEndpoint::Entity(target.id)),
            "{from}: {:?}",
            calls(&output)
        );
    }
}

#[test]
fn quoted_discarded_reader_and_documentary_data_never_become_bindings_or_calls() {
    let source = "(ns Sample) (defn target [x] x) '(defn quoted [x] (target x)) #_(defn discarded [x] (target x)) (comment (defn commented [x] (target x))) (defn caller [x] ['(target x) `(target x) #_(target x) \"(defn textual [] (target x))\"] (target x))";
    let output = analyze(source);
    for name in ["quoted", "discarded", "commented", "textual"] {
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|e| e.canonical_name == name)
        );
    }
    let caller = entity(&output, "caller");
    let target = entity(&output, "target");
    let call = calls(&output);
    assert_eq!(call.len(), 1);
    assert_eq!(call[0].subject, RelationEndpoint::Entity(caller.id));
    assert_eq!(call[0].object, RelationEndpoint::Entity(target.id));
    assert_eq!(
        call[0]
            .evidence
            .source
            .as_ref()
            .unwrap()
            .span()
            .start_byte(),
        u64::try_from(source.rfind("(target x)").unwrap() + 1).unwrap()
    );
}

#[test]
fn parameters_let_shadowing_and_duplicate_targets_withhold_guessed_calls() {
    for source in [
        "(ns Sample) (defn target [x] x) (defn caller [target] (target 1))",
        "(ns Sample) (defn target [x] x) (defn caller [x] (let [target x] (target 1)))",
        "(ns Sample) (defn target [x] x) (defn target [x] x) (defn caller [x] (target x))",
        "(ns Sample) (defn target [x] x) (defn caller [x] (unknown-macro (target x)))",
    ] {
        let output = analyze(source);
        assert!(calls(&output).is_empty(), "{source}: {:?}", calls(&output));
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|g| g.detail == "clojure-written-binding-or-call-target-unavailable")
        );
    }
}

#[test]
fn dynamic_namespace_and_rebound_core_forms_remain_explicit_gaps() {
    for source in [
        "(ns Sample) (defn (computed-name) [] 1)",
        "(ns Sample) (in-ns computed) (defn hidden [] 1)",
        "(ns Sample (:refer-clojure :exclude [defn])) (defn hidden [] 1)",
    ] {
        let output = analyze(source);
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|e| e.kind == EntityKind::Function)
        );
        assert!(!output.document().skipped_regions.is_empty());
    }
    let malformed = analyze("(ns Sample) (defn broken [x] (target x)");
    assert!(
        !malformed
            .document()
            .entities
            .iter()
            .any(|e| e.canonical_name == "broken")
    );
}

#[test]
fn multiple_arities_and_private_functions_keep_one_written_owner() {
    let source =
        "(ns Sample) (defn- target [x] x) (defn caller ([x] [(target x)]) ([x y] (target y)))";
    let output = analyze(source);
    let owner = entity(&output, "caller");
    let target = entity(&output, "target");
    assert_eq!(
        calls(&output)
            .iter()
            .filter(|r| r.subject == RelationEndpoint::Entity(owner.id)
                && r.object == RelationEndpoint::Entity(target.id))
            .count(),
        2
    );
}

#[test]
fn evaluated_namespace_rebinding_does_not_keep_a_stale_function_target() {
    let output = analyze(
        "(ns Sample) (defn target [x] x) (defn caller [x] (def target identity) (target x))",
    );
    let _ = entity(&output, "target");
    let _ = entity(&output, "caller");
    assert!(
        calls(&output).is_empty(),
        "an explicit evaluated rebind must leave the target unknown"
    );
}

#[test]
fn evaluated_namespace_switch_does_not_attach_later_definitions_to_a_stale_owner() {
    let output = analyze("(ns B) (ns A) (do (in-ns 'B)) (defn target [] 1)");
    for name in ["A", "B"] {
        assert!(
            output
                .document()
                .entities
                .iter()
                .any(|e| e.kind == EntityKind::Namespace && e.canonical_name == name)
        );
    }
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|e| e.kind == EntityKind::Function && e.canonical_name == "target"),
        "an evaluated namespace barrier leaves the later owner unknown"
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "clojure-namespace-owner-unavailable")
    );
    for source in [
        "(ns A) '(in-ns 'B) (defn target [] 1)",
        "(ns A) (defn delayed [] (in-ns 'B)) (defn target [] 1)",
        "(ns B) (ns A) (do (in-ns 'B)) (ns A) (defn target [] 1)",
    ] {
        let retained = analyze(source);
        let target = entity(&retained, "target");
        let owners = retained
            .document()
            .relations
            .iter()
            .filter(|r| {
                r.predicate == RelationPredicate::Contains
                    && r.object == RelationEndpoint::Entity(target.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(owners.len(), 1);
        assert!(
            retained
                .document()
                .entities
                .iter()
                .any(|e| e.kind == EntityKind::Namespace
                    && e.canonical_name == "A"
                    && owners[0].subject == RelationEndpoint::Entity(e.id))
        );
    }
}

#[test]
fn utf16_unicode_prefixes_preserve_physical_definition_and_vector_call_identity() {
    let source =
        "; λ🙂 raw prefix\n(ns Sample)\n(defn target? [x] x)\n(defn caller [x] [(target? x)])\n";
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        raw.extend(source.encode_utf16().flat_map(|unit| {
            if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            }
        }));
        let output = analyze_raw(&raw, if little { "utf-16le" } else { "utf-16be" });
        let target = entity(&output, "target?");
        let owner = entity(&output, "caller");
        let call = calls(&output);
        let relation = call
            .iter()
            .find(|r| {
                r.subject == RelationEndpoint::Entity(owner.id)
                    && r.object == RelationEndpoint::Entity(target.id)
            })
            .unwrap();
        let witness = relation.evidence.source.as_ref().unwrap();
        let start = source.rfind("target?").unwrap();
        let raw_start = 2 + 2 * source[..start].encode_utf16().count();
        let expected = "target?"
            .encode_utf16()
            .flat_map(|unit| {
                if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            witness.span().start_byte(),
            u64::try_from(raw_start).unwrap()
        );
        assert_eq!(
            witness.span().end_byte(),
            u64::try_from(raw_start + expected.len()).unwrap()
        );
        assert_eq!(&raw[raw_start..raw_start + expected.len()], expected);
        assert_eq!(witness.content_hash(), content_hash(&raw));
        assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
        let definition_start = 2 + 2 * source[..source.find("target?").unwrap()]
            .encode_utf16()
            .count();
        let definitions = output
            .document()
            .occurrences
            .iter()
            .filter(|o| {
                o.role == OccurrenceRole::Definition
                    && o.target == OccurrenceTarget::Resolved { symbol: target.id }
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        let definition = definitions[0];
        assert_eq!(
            definition.source.span().start_byte(),
            u64::try_from(definition_start).unwrap()
        );
        assert_eq!(
            definition.source.span().end_byte(),
            u64::try_from(definition_start + expected.len()).unwrap()
        );
        assert_eq!(
            &raw[definition_start..definition_start + expected.len()],
            expected
        );
        assert_eq!(definition.source.content_hash(), content_hash(&raw));
        assert_eq!(definition.syntactic_text_hash, content_hash(b"target?"));
        let namespace = output
            .document()
            .entities
            .iter()
            .find(|e| e.kind == EntityKind::Namespace && e.canonical_name == "Sample")
            .unwrap();
        assert!(
            output
                .document()
                .relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Contains
                    && r.subject == RelationEndpoint::Entity(namespace.id)
                    && r.object == RelationEndpoint::Entity(target.id))
        );
    }
}
