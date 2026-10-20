//! Written C calls require native same-file function and lexical binding proof.
//! These controls preserve uncertain shadowed, indirect and preprocessor sites.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ResourceKind, SinkError,
    StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, EntityKind, ExtensionSupport, FactRef, IrLimits,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate,
    SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = concat!(
    "struct heap { int count; };\n",
    "struct loop { struct heap timers; };\n",
    "struct timer { struct loop *loop; int repeat; };\n",
    "void heap_insert(struct heap *heap, int value);\n",
    "static struct heap *timer_heap(const struct loop *loop) { return (struct heap *)&loop->timers; }\n",
    "int timer_start(struct timer *handle, int value) { heap_insert(timer_heap(handle->loop), value); return 0; }\n",
    "int timer_again(struct timer *handle) { return timer_start(handle, handle->repeat); }\n",
);

fn analyze(text: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    analyze_raw(text.as_bytes(), "utf-8")
}

fn analyze_raw(raw: &[u8], encoding: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    execute(raw, encoding, 4096).unwrap().0
}

fn execute(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
) -> Result<(rootlight_adapter_sdk::AnalysisOutput, usize), AdapterError> {
    execute_language(raw, encoding, maximum, "c")
}

fn execute_language(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
    language: &str,
) -> Result<(rootlight_adapter_sdk::AnalysisOutput, usize), AdapterError> {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.c"), raw).unwrap();
    let repository = derive_repository(b"c-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.c")).unwrap(),
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
    let provider = Arc::new(
        TreeSitterProvider::new(
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
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "c-source-contract",
            "1.0",
            content_hash(b"c-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new(language).unwrap(),
        "c-static-contract",
        content_hash(b"c-contract-binary"),
    )
    .unwrap();
    let batch = BatchThresholds::new(maximum.min(64), 1024 * 1024, 32, 16 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        maximum,
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
        stream,
        StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap(),
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new(language).unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"c-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let required =
        provider.required_syntax_fact_count(&request.to_parse_request(), &Cancellation::new())?;
    let result = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )?;
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), content_hash(raw));
        assert_eq!(occurrence.source.repository(), repository);
        assert_eq!(occurrence.source.generation(), source.generation());
        assert!(occurrence.source.span().end_byte() <= u64::try_from(raw.len()).unwrap());
    }
    Ok((result, required))
}

fn entity_at(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    name: &str,
    kind: EntityKind,
    start: usize,
    length: usize,
) -> rootlight_ids::SymbolId {
    let definitions = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                && occurrence.source.span().end_byte() == u64::try_from(start + length).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        definitions.len(),
        1,
        "{name}: {:?}",
        output.document().occurrences
    );
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("definition must be resolved")
    };
    let entity = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.display_name, name);
    assert_eq!(
        definitions[0].syntactic_text_hash,
        content_hash(name.as_bytes())
    );
    symbol
}

fn assert_call(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    caller: rootlight_ids::SymbolId,
    target: rootlight_ids::SymbolId,
    start: usize,
    end: usize,
) {
    let document = output.document();
    let occurrences: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.target == OccurrenceTarget::Resolved { symbol: target }
        })
        .collect();
    assert_eq!(occurrences.len(), 1);
    assert_eq!(occurrences[0].syntax_kind, "c.direct_file_call.call");
    assert_eq!(occurrences[0].enclosing, Some(caller));
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(caller)
                && edge.object == RelationEndpoint::Entity(target)
        })
        .collect();
    assert_eq!(edges.len(), 1, "written same-file C call must be present");
    let edge = edges[0];
    assert_eq!(edge.id, derive_relation_record_id(edge).unwrap());
    let mut expected = vec![
        FactRef::Entity(caller),
        FactRef::Fact(occurrences[0].id),
        FactRef::Entity(target),
    ];
    expected.sort_unstable();
    expected.dedup();
    assert_eq!(expected.len(), 3);
    assert_eq!(edge.evidence.derivation, expected);
    let source = edge.evidence.source.as_ref().unwrap();
    assert_eq!(source, &occurrences[0].source);
    assert!(source.span().start_byte() <= u64::try_from(start).unwrap());
    assert!(source.span().end_byte() >= u64::try_from(end).unwrap());
}

#[test]
fn direct_written_function_calls_have_exact_same_file_targets_and_canonical_proof() {
    let output = analyze(SOURCE);
    let function = |name: &str, marker: &str| {
        let start = SOURCE.find(marker).unwrap() + marker.find(name).unwrap();
        entity_at(&output, name, EntityKind::Function, start, name.len())
    };
    let heap = function("timer_heap", "*timer_heap(");
    let start = function("timer_start", "int timer_start(");
    let again = function("timer_again", "int timer_again(");
    for (caller, target, written) in [
        (start, heap, "timer_heap(handle->loop)"),
        (again, start, "timer_start(handle, handle->repeat)"),
    ] {
        let offset = SOURCE.find(written).unwrap();
        assert_call(&output, caller, target, offset, offset + written.len());
    }
    assert_eq!(
        output
            .document()
            .relations
            .iter()
            .filter(|edge| { edge.predicate == RelationPredicate::Calls })
            .count(),
        2
    );
}

#[test]
fn lexical_preprocessor_and_indirect_call_barriers_never_gain_exact_edges() {
    for source in [
        "int target(void){return 1;} int caller(int (*target)(void)){return target();}",
        "int target(void){return 1;} int caller(void){int (*target)(void)=0;return target();}",
        "int target(void){return 1;} int caller(void){typedef int target;return target();}",
        "int target(void){return 1;} int caller(void){enum {target=0};return target();}",
        "int target(void){return 1;}\n#define target(...) other(__VA_ARGS__)\nint caller(void){return target();}",
        "int target(void){return 1;}\n#define target other\nint caller(void){return target();}",
        "int target(void){return 1;}\n#define BIND(name) int (*name)(void)=0\nint caller(void){BIND(target);return target();}",
        "int target(void){return 1;}\n#define BIND int (*target)(void)=0\nint caller(void){BIND;return target();}",
        "#if FLAG\nint target(void){return 1;}\n#endif\nint caller(void){return target();}",
        "int target(void){return 1;} int caller(void){\n#if FLAG\nreturn target();\n#endif\nreturn 0;}",
        "int target(void){return 1;}\n#if FLAG\nint caller(void){return target();}\n#endif\n",
        "int target(void){return 1;} int target(void){return 2;} int caller(void){return target();}",
        "int target(void); int caller(void){return target();}",
        "int target(void){return 1;} int caller(void){return (target)();}",
        "int target(void){return 1;} struct Receiver{int (*target)(void);}; int caller(struct Receiver *r){return r->target();}",
        "int target(void){return 1;} int caller(void){target=0;return target();}",
        "int target(void){return 1;} int caller(void){int value=target();int target=0;return value;}",
        "int caller(void){return target();} int target(void){return 1;}",
    ] {
        let output = analyze(source);
        assert!(
            !output
                .document()
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "syntax-error-recovery" }),
            "negative binding fixture must remain native-syntax-valid: {source}"
        );
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|edge| { edge.predicate == RelationPredicate::Calls }),
            "{source}"
        );
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| { site.syntax_kind == "c.direct_file_call.call" }),
            "{source}"
        );
    }
}

#[test]
fn other_function_bindings_and_field_names_do_not_shadow_a_direct_c_call() {
    let source = concat!(
        "#define UNRELATED 7\n",
        "int target(void){return 1;}\n",
        "int other(int target){return target;}\n",
        "struct Receiver{int target;};\n",
        "int caller(void){return target();}\n",
    );
    let output = analyze(source);
    let target_start = source.find("target(void)").unwrap();
    let target = entity_at(&output, "target", EntityKind::Function, target_start, 6);
    let caller_start = source.find("caller(void)").unwrap();
    let caller = entity_at(&output, "caller", EntityKind::Function, caller_start, 6);
    let start = source.find("target();").unwrap();
    assert_call(&output, caller, target, start, start + 8);
    let cpp = execute_language(b"int target(){return 1;} class Receiver { public: virtual int target(); }; int caller(Receiver *r){return r->target();}", "utf-8", 4096, "cpp").unwrap().0;
    assert!(
        !cpp.document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn utf16_calls_retain_exact_physical_spans_hashes_and_proof() {
    let source = "/* λ🙂 */\nint target(void){return 1;} int caller(void){return target();}\n";
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
        let physical = |offset| 2 + 2 * source[..offset].encode_utf16().count();
        let target = entity_at(
            &output,
            "target",
            EntityKind::Function,
            physical(source.find("target(void)").unwrap()),
            12,
        );
        let caller = entity_at(
            &output,
            "caller",
            EntityKind::Function,
            physical(source.find("caller(void)").unwrap()),
            12,
        );
        let start = physical(source.find("target();").unwrap());
        assert_call(&output, caller, target, start, start + 16);
        let expected: Vec<_> = "target()"
            .encode_utf16()
            .flat_map(|unit| {
                if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect();
        assert_eq!(&raw[start..start + 16], expected);
    }
}

#[test]
fn required_fact_admission_stays_atomic_with_optional_native_call_proofs() {
    let (_, required) = execute(SOURCE.as_bytes(), "utf-8", 4096).unwrap();
    assert!(required > 1);
    assert_eq!(
        execute(SOURCE.as_bytes(), "utf-8", required - 1).unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1
        })
    );
}
