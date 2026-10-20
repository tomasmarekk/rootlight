//! D analysis retains written headers, physical names and lexical containment.
//! Constraint evaluation, imported bindings and computed labels remain gaps.

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
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, IrLimits,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate,
    SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = include_str!("fixtures/d/index.d");

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
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.d"), raw).unwrap();
    let repository = derive_repository(b"d-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.d")).unwrap(),
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
            "d-source-contract",
            "1.0",
            content_hash(b"d-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("d").unwrap(),
        "d-static-contract",
        content_hash(b"d-contract-binary"),
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
        LanguageId::new("d").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"d-contract-build-context")),
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

fn contains(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    owner: rootlight_ids::SymbolId,
    member: rootlight_ids::SymbolId,
    name_start: usize,
    name_end: usize,
) {
    let matches = output
        .document()
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner)
                && relation.object == RelationEndpoint::Entity(member)
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1);
    let relation = matches[0];
    let reference = relation.evidence.source.as_ref().unwrap();
    assert!(reference.span().start_byte() <= u64::try_from(name_start).unwrap());
    assert!(reference.span().end_byte() >= u64::try_from(name_end).unwrap());
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
}

#[test]
fn original_module_function_parameters_and_three_contains_keep_physical_witnesses() {
    assert_eq!(SOURCE.len(), 191);
    let output = analyze(SOURCE);
    let module = entity_at(&output, "index", EntityKind::Module, 7, 5);
    let function = entity_at(&output, "showHome", EntityKind::Function, 52, 8);
    let req = entity_at(&output, "req", EntityKind::Parameter, 79, 3);
    let res = entity_at(&output, "res", EntityKind::Parameter, 103, 3);
    contains(&output, module, function, 52, 60);
    contains(&output, function, req, 79, 82);
    contains(&output, function, res, 103, 106);
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::Definition
                && o.source.span().start_byte() == 171
                && o.source.span().end_byte() == 174)
    );
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|r| r.predicate == RelationPredicate::Calls)
    );
    assert!(output.document().skipped_regions.iter().any(
        |g| g.detail == "d-template-ctfe-type-compiler-binding-and-resolved-calls-unavailable"
    ));
}
#[test]
fn aggregate_conditional_template_body_and_default_values_do_not_donate_headers() {
    let source = concat!(
        "module sample;\n",
        "void live(int = fallback, int named = value) { void nested() {} }\n",
        "class Record { void method(int member) {} }\n",
        "version(Enabled) { void conditional() {} }\n",
        "mixin template Generated() { void generated() {} }\n",
        "string syntax = q{ void phantom(int fake) {} };\n"
    );
    let output = analyze(source);
    entity_at(
        &output,
        "live",
        EntityKind::Function,
        source.find("live").unwrap(),
        4,
    );
    entity_at(
        &output,
        "named",
        EntityKind::Parameter,
        source.find("named").unwrap(),
        5,
    );
    for name in [
        "fallback",
        "value",
        "nested",
        "Record",
        "method",
        "member",
        "conditional",
        "Generated",
        "generated",
        "phantom",
        "fake",
    ] {
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|e| e.canonical_name == name),
            "{name}"
        );
    }
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|g| g.detail == "d-static-header-or-written-owner-proof-unavailable")
    );
}
#[test]
fn both_utf16_orders_preserve_module_parameter_leaves_and_exact_raw_content() {
    let source = "// λ🙂 prefix\nmodule sample;\nvoid measure(int value) {}\n";
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        raw.extend(source.encode_utf16().flat_map(|u| {
            if little {
                u.to_le_bytes()
            } else {
                u.to_be_bytes()
            }
        }));
        let output = analyze_raw(&raw, if little { "utf-16le" } else { "utf-16be" });
        let start = |n: &str| 2 + 2 * source[..source.find(n).unwrap()].encode_utf16().count();
        let module = entity_at(&output, "sample", EntityKind::Module, start("sample"), 12);
        let function = entity_at(
            &output,
            "measure",
            EntityKind::Function,
            start("measure"),
            14,
        );
        let parameter = entity_at(&output, "value", EntityKind::Parameter, start("value"), 10);
        contains(
            &output,
            module,
            function,
            start("measure"),
            start("measure") + 14,
        );
        contains(
            &output,
            function,
            parameter,
            start("value"),
            start("value") + 10,
        );
        for name in ["sample", "measure", "value"] {
            let bytes = name
                .encode_utf16()
                .flat_map(|u| {
                    if little {
                        u.to_le_bytes()
                    } else {
                        u.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(&raw[start(name)..start(name) + bytes.len()], bytes);
        }
        assert_eq!(output.document().files[0].content_hash, content_hash(&raw));
    }
}
#[test]
fn body_edit_identity_and_exact_fact_admission_remain_bounded() {
    let source = "module sample; void measure(int value) { int local = 1; }\n";
    let a = analyze(source);
    let b = analyze(&source.replace("= 1", "= 2"));
    for (name, kind) in [
        ("sample", EntityKind::Module),
        ("measure", EntityKind::Function),
        ("value", EntityKind::Parameter),
    ] {
        let start = source.find(name).unwrap();
        assert_eq!(
            entity_at(&a, name, kind, start, name.len()),
            entity_at(&b, name, kind, start, name.len())
        );
    }
    let (_, required) = execute(source.as_bytes(), "utf-8", 4096).unwrap();
    assert!(required > 1);
    assert_eq!(
        execute(source.as_bytes(), "utf-8", required).unwrap().1,
        required
    );
    assert_eq!(
        execute(source.as_bytes(), "utf-8", required - 1).unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1
        })
    );
    assert!(
        !analyze("module sample; void broken( {")
            .document()
            .skipped_regions
            .is_empty()
    );
}
