//! Public Nim analysis preserves written bindings and direct lexical calls.
//! Compile-time expansion, imported overloads and dynamic targets remain explicit gaps.

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

const SOURCE: &str = include_str!("fixtures/nim/patterns.nim");

fn analyze(text: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    analyze_raw(text.as_bytes(), "utf-8")
}

fn analyze_raw(raw: &[u8], encoding: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    analyze_path(raw, encoding, "source.nim")
}

fn analyze_path(raw: &[u8], encoding: &str, path: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join(path), raw).unwrap();
    let repository = derive_repository(b"nim-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 1024 * 1024)
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
            "nim-source-contract",
            "1.0",
            content_hash(b"nim-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("nim").unwrap(),
        "nim-static-contract",
        content_hash(b"nim-contract-binary"),
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
        LanguageId::new("nim").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"nim-contract-build-context")),
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
    kind: EntityKind,
) -> &'a rootlight_ir::EntityRecord {
    let values = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name && entity.kind == kind)
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

fn definition(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    name: &str,
    kind: EntityKind,
    start: u64,
    end: u64,
) {
    let owner = entity(output, name, kind);
    let values = output
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
        values.len(),
        1,
        "{name}: {:?}",
        output.document().occurrences
    );
    assert_eq!(
        values[0].target,
        OccurrenceTarget::Resolved { symbol: owner.id }
    );
    assert_eq!(values[0].syntactic_text_hash, content_hash(name.as_bytes()));
}

fn contains(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    from: &str,
    from_kind: EntityKind,
    to: &str,
    to_kind: EntityKind,
) {
    let owner = entity(output, from, from_kind);
    let target = entity(output, to, to_kind);
    assert_eq!(
        output
            .document()
            .relations
            .iter()
            .filter(|r| r.predicate == RelationPredicate::Contains
                && r.subject == RelationEndpoint::Entity(owner.id)
                && r.object == RelationEndpoint::Entity(target.id))
            .count(),
        1
    );
}

fn assert_call(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    from: &str,
    to: &str,
    start: u64,
    end: u64,
) {
    let owner = entity(output, from, EntityKind::Function);
    let target = entity(output, to, EntityKind::Function);
    let relations = calls(output)
        .into_iter()
        .filter(|r| {
            r.subject == RelationEndpoint::Entity(owner.id)
                && r.object == RelationEndpoint::Entity(target.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        relations.len(),
        1,
        "{from}->{to}: {:?}",
        output.document().relations
    );
    let relation = relations[0];
    let witness = relation.evidence.source.as_ref().unwrap();
    assert_eq!(
        (witness.span().start_byte(), witness.span().end_byte()),
        (start, end)
    );
    let occurrences = output
        .document()
        .occurrences
        .iter()
        .filter(|o| {
            o.role == OccurrenceRole::CallSite
                && o.enclosing == Some(owner.id)
                && o.target == (OccurrenceTarget::Resolved { symbol: target.id })
                && o.source == *witness
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
fn jester_patterns_preserves_authored_definitions_field_owner_and_direct_call() {
    assert_eq!(SOURCE.len(), 4453);
    let output = analyze(SOURCE);
    for (name, kind, start, end) in [
        ("Node", EntityKind::Struct, 173, 177),
        ("typ", EntityKind::Field, 193, 196),
        ("findNextText", EntityKind::Function, 1950, 1962),
        ("match", EntityKind::Function, 2385, 2390),
    ] {
        assert_eq!(
            &SOURCE[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
            name
        );
        definition(&output, name, kind, start, end);
    }
    contains(
        &output,
        "Node",
        EntityKind::Struct,
        "typ",
        EntityKind::Field,
    );
    assert_call(&output, "match", "findNextText", 3170, 3182);
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::Definition && o.source.span().start_byte() == 3170)
    );
    assert_eq!(
        SOURCE
            .match_indices("findNextText")
            .map(|(index, _)| index)
            .collect::<Vec<_>>(),
        [1950, 3170]
    );
    assert!(!SOURCE.contains("findNextText__absent_source_control__"));
}

#[test]
fn style_equivalence_keeps_written_spans_and_first_character_case_distinct() {
    let source = "proc findNextText() = discard\nproc caller() = find_next_text()\nproc other() = FindNextText()\n";
    let output = analyze(source);
    let start = source.find("find_next_text").unwrap();
    assert_call(
        &output,
        "caller",
        "findNextText",
        u64::try_from(start).unwrap(),
        u64::try_from(start + 14).unwrap(),
    );
    assert_eq!(calls(&output).len(), 1);
    definition(&output, "findNextText", EntityKind::Function, 5, 17);
    let ambiguous = analyze(
        "proc targetName() = discard\nproc target_name(x: int) = discard\nproc caller() = targetName()\n",
    );
    assert!(calls(&ambiguous).is_empty());
}

#[test]
fn parameter_local_and_unmodeled_binding_shadowing_never_invents_a_direct_call() {
    for source in [
        "proc target() = discard\nproc caller(target: proc()) = target()\n",
        "proc target() = discard\nproc caller() =\n  let target = proc() = discard\n  target()\n",
        "proc target() = discard\nproc caller() =\n  template target(): untyped = discard\n  target()\n",
        "proc target() = discard\nwhen selected:\n  proc hidden() = target()\n",
        "proc target() = discard\nproc caller() = unknownMacro(target())\n",
        "proc target() = discard\nwhen selected:\n  proc target(value: int) = discard\nproc caller() = target()\n",
        "proc targetName() = discard\nwhen selected:\n  var target_name: proc()\nproc caller() = targetName()\n",
    ] {
        let output = analyze(source);
        assert!(calls(&output).is_empty(), "{source}: {:?}", calls(&output));
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail.starts_with("nim-"))
        );
    }
}

#[test]
fn comments_strings_dynamic_names_and_incomplete_owners_do_not_become_definitions() {
    let output = analyze(
        "# proc ghost() = discard\nlet text = \"proc phantom() = discard\"\nproc `+`(a, b: int): int = a\nproc live() = discard\n",
    );
    assert!(
        output
            .document()
            .entities
            .iter()
            .all(|entity| !["ghost", "phantom", "+"].contains(&entity.canonical_name.as_str()))
    );
    let _ = entity(&output, "live", EntityKind::Function);
    let malformed = analyze("proc broken( =\nproc live() = discard\n");
    assert!(
        !malformed
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "broken")
    );
    assert_ne!(
        malformed.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn grouped_object_fields_remain_distinct_exact_leaves_without_export_markers() {
    let source = "type\n  Pair* = object\n    left*, right*: int\n";
    let output = analyze(source);
    for field in ["left", "right"] {
        let start = source.find(field).unwrap();
        definition(
            &output,
            field,
            EntityKind::Field,
            u64::try_from(start).unwrap(),
            u64::try_from(start + field.len()).unwrap(),
        );
        contains(
            &output,
            "Pair",
            EntityKind::Struct,
            field,
            EntityKind::Field,
        );
    }
}

#[test]
fn utf16_both_orders_preserve_object_field_and_call_physical_source_identity() {
    let source = "# λ🙂 prefix\ntype\n  Node* = object\n    typ*: int\nproc target() = discard\nproc caller() = target()\n";
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
        for (name, marker, kind) in [
            ("Node", "Node*", EntityKind::Struct),
            ("typ", "typ*", EntityKind::Field),
            ("target", "target(", EntityKind::Function),
        ] {
            let start = 2 + 2 * source[..source.find(marker).unwrap()]
                .encode_utf16()
                .count();
            definition(
                &output,
                name,
                kind,
                u64::try_from(start).unwrap(),
                u64::try_from(start + 2 * name.len()).unwrap(),
            );
            let expected = name
                .encode_utf16()
                .flat_map(|unit| {
                    if little {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(&raw[start..start + expected.len()], expected);
        }
        contains(
            &output,
            "Node",
            EntityKind::Struct,
            "typ",
            EntityKind::Field,
        );
        let start = 2 + 2 * source[..source.rfind("target").unwrap()]
            .encode_utf16()
            .count();
        assert_call(
            &output,
            "caller",
            "target",
            u64::try_from(start).unwrap(),
            u64::try_from(start + 12).unwrap(),
        );
    }
}

#[test]
fn nimscript_files_keep_exact_source_without_claiming_task_or_compiler_evaluation() {
    for (path, raw, length) in [
        (
            "config.nims",
            include_bytes!("fixtures/nim/config.nims").as_slice(),
            43,
        ),
        (
            "jester.nimble",
            include_bytes!("fixtures/nim/jester.nimble").as_slice(),
            482,
        ),
    ] {
        assert_eq!(raw.len(), length);
        let output = analyze_path(raw, "utf-8", path);
        assert!(calls(&output).is_empty());
        assert!(
            output.document().entities.iter().all(|entity| ![
                "version",
                "author",
                "description",
                "license",
                "skipFiles",
                "skipDirs",
            ]
            .contains(&entity.canonical_name.as_str())),
            "Nimble assignments target existing DSL attributes"
        );
        for domain in [
            rootlight_ir::FactDomain::Entities,
            rootlight_ir::FactDomain::Occurrences,
            rootlight_ir::FactDomain::Relations,
        ] {
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.domain == domain && gap.detail.starts_with("nim-"))
            );
        }
    }
}

#[test]
fn block_local_function_does_not_escape_its_written_scope() {
    let source = "proc target() = discard\nproc caller() =\n  block:\n    proc target() = discard\n    target()\n  target()\n";
    let output = analyze(source);
    let caller = entity(&output, "caller", EntityKind::Function);
    let module_target = output
        .document()
        .entities
        .iter()
        .find(|e| {
            e.canonical_name == "target"
                && e.evidence.source.as_ref().unwrap().span().start_byte() == 0
        })
        .unwrap();
    let edges = calls(&output);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].subject, RelationEndpoint::Entity(caller.id));
    assert_eq!(edges[0].object, RelationEndpoint::Entity(module_target.id));
    assert_eq!(
        edges[0]
            .evidence
            .source
            .as_ref()
            .unwrap()
            .span()
            .start_byte(),
        u64::try_from(source.rfind("target()").unwrap()).unwrap()
    );
    let inside = source.find("    target()").unwrap() + 4;
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::CallSite
                && occurrence.source.span().start_byte() == u64::try_from(inside).unwrap()
                && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn quoted_nim_ast_does_not_publish_generated_procedure_definitions() {
    let output = analyze(
        "import std/macros\nproc makeAst(): NimNode {.compileTime.} =\n  result = quote do:\n    proc phantom() = discard\n",
    );
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "phantom")
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("nim-"))
    );
    let source = "import std/macros\nproc target() = discard\nproc makeAst(): NimNode {.compileTime.} =\n  result = quote do:\n    proc target() = discard\n    let fake = 1\nproc caller() = target()\n";
    let retained = analyze(source);
    let start = source.rfind("target()").unwrap();
    assert_call(
        &retained,
        "caller",
        "target",
        u64::try_from(start).unwrap(),
        u64::try_from(start + 6).unwrap(),
    );
}

#[test]
fn imported_overload_does_not_resolve_to_an_incompatible_local_literal_parameter() {
    let output = analyze(
        "import std/strutils\nproc parseInt(x: int): int = x\nproc caller() = discard parseInt(\"42\")\n",
    );
    let _ = entity(&output, "parseInt", EntityKind::Function);
    assert!(calls(&output).is_empty());
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::CallSite
                && matches!(o.target, OccurrenceTarget::Unresolved { .. }))
    );
    for source in [
        "import std/strutils\nproc target() = discard\nproc caller() = target()\n",
        "proc target() = discard\nwhen selected:\n  var unrelated: int\nproc caller() = target()\n",
    ] {
        let retained = analyze(source);
        let start = source.rfind("target()").unwrap();
        assert_call(
            &retained,
            "caller",
            "target",
            u64::try_from(start).unwrap(),
            u64::try_from(start + 6).unwrap(),
        );
    }
}

#[test]
fn imported_overload_keeps_typed_parameter_argument_unresolved() {
    let source = "import std/strutils\nproc parseInt(x: int): int = x\nproc caller(s: string) = discard parseInt(s)\n";
    let output = analyze(source);
    let _ = entity(&output, "parseInt", EntityKind::Function);
    assert!(calls(&output).is_empty());
    let start = u64::try_from(source.rfind("parseInt").unwrap()).unwrap();
    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.source.span().start_byte() == start
            && occurrence.source.span().end_byte() == start + 8
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    }));
    let compatible = "import std/strutils\nproc parseInt(x: int): int = x\nproc caller(n: int) = discard parseInt(n)\n";
    let retained = analyze(compatible);
    let start = u64::try_from(compatible.rfind("parseInt").unwrap()).unwrap();
    assert_call(&retained, "caller", "parseInt", start, start + 8);
}

#[test]
fn imported_overload_keeps_variable_bound_argument_unresolved() {
    for binding in [
        "let s = \"42\"",
        "var s: string = \"42\"",
        "let original = \"42\"\n  let s = original",
        "var s: string",
    ] {
        let source = format!(
            "import std/strutils\nproc parseInt(x: int): int = x\nproc caller() =\n  {binding}\n  discard parseInt(s)\n"
        );
        let output = analyze(&source);
        let _ = entity(&output, "parseInt", EntityKind::Function);
        assert!(calls(&output).is_empty(), "{binding}");
        let start = u64::try_from(source.rfind("parseInt").unwrap()).unwrap();
        assert!(output.document().occurrences.iter().any(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.source.span().start_byte() == start
                && occurrence.source.span().end_byte() == start + 8
                && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
        }));
    }
}
