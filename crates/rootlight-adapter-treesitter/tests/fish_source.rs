//! Fish analysis retains written functions, named arguments and source containment.
//! Expansion, conditional declarations and runtime command binding remain gaps.

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

const SOURCE: &str = include_str!("fixtures/fish/fisher.fish");

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
    fs::write(directory.path().join("source.fish"), raw).unwrap();
    let repository = derive_repository(b"fish-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.fish")).unwrap(),
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
            "fish-source-contract",
            "1.0",
            content_hash(b"fish-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("fish").unwrap(),
        "fish-static-contract",
        content_hash(b"fish-contract-binary"),
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
        LanguageId::new("fish").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"fish-contract-build-context")),
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

fn entity<'a>(
    output: &'a rootlight_adapter_sdk::AnalysisOutput,
    name: &str,
    kind: EntityKind,
) -> &'a rootlight_ir::EntityRecord {
    let matches = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name && entity.kind == kind)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{name}: {:?}", output.document().entities);
    matches[0]
}

fn definition<'a>(
    output: &'a rootlight_adapter_sdk::AnalysisOutput,
    entity: &rootlight_ir::EntityRecord,
) -> &'a rootlight_ir::OccurrenceRecord {
    let matches = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == (OccurrenceTarget::Resolved { symbol: entity.id })
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{}", entity.canonical_name);
    matches[0]
}

#[test]
fn original_function_named_argument_and_contains_keep_exact_witnesses() {
    let output = analyze(SOURCE);
    let function = entity(&output, "fisher", EntityKind::Function);
    let argument = entity(&output, "cmd", EntityKind::Parameter);
    for (record, start, end) in [(function, 9, 15), (argument, 33, 36)] {
        let occurrence = definition(&output, record);
        assert_eq!(occurrence.source.span().start_byte(), start);
        assert_eq!(occurrence.source.span().end_byte(), end);
        assert_eq!(
            &SOURCE[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
            record.canonical_name
        );
    }
    let contains = output
        .document()
        .relations
        .iter()
        .find(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(function.id)
                && relation.object == RelationEndpoint::Entity(argument.id)
        })
        .unwrap();
    assert_eq!(
        contains.evidence.source.as_ref().unwrap(),
        &definition(&output, argument).source
    );
    assert_eq!(contains.id, derive_relation_record_id(contains).unwrap());
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "fish-runtime-command-source-eval-and-rebinding-unavailable")
    );
}

#[test]
fn option_values_documentary_text_and_dynamic_contexts_do_not_fabricate_parameters() {
    let source = "# function commented -a phantom\nfunction literal --description 'parameter-looking' -a first second\n echo 'function string_only -a phantom'\n function nested -a hidden\n  echo ok\n end\nend\nif true\n function conditional -a hidden\n  echo ok\n end\nend\nfunction $dynamic -a hidden\n echo ok\nend\nfunction unknown --unsupported value -a hidden\n echo ok\nend\n";
    let output = analyze(source);
    entity(&output, "literal", EntityKind::Function);
    entity(&output, "first", EntityKind::Parameter);
    entity(&output, "second", EntityKind::Parameter);
    for absent in [
        "parameter-looking",
        "commented",
        "phantom",
        "string_only",
        "nested",
        "conditional",
        "dynamic",
        "unknown",
        "hidden",
    ] {
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == absent),
            "{absent}: {:?}",
            output.document().entities
        );
    }
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "fish-function-name-header-or-execution-context-unavailable")
    );
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|row| row.role == OccurrenceRole::Documentation)
    );
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|row| row.role == OccurrenceRole::StringEvidence)
    );
}

#[test]
fn utf16_definitions_and_contains_retain_raw_source_identity() {
    let source = "# λ🙂 prefix\nfunction Written -a argument\n echo $argument\nend\n";
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
        let function = entity(&output, "Written", EntityKind::Function);
        let argument = entity(&output, "argument", EntityKind::Parameter);
        for record in [function, argument] {
            let occurrence = definition(&output, record);
            let start = source.find(&record.canonical_name).unwrap();
            let raw_start = 2 + 2 * source[..start].encode_utf16().count();
            let raw_end = raw_start + 2 * record.canonical_name.encode_utf16().count();
            assert_eq!(
                occurrence.source.span().start_byte(),
                u64::try_from(raw_start).unwrap()
            );
            assert_eq!(
                occurrence.source.span().end_byte(),
                u64::try_from(raw_end).unwrap()
            );
            assert_eq!(occurrence.source.content_hash(), content_hash(&raw));
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(record.canonical_name.as_bytes())
            );
        }
        let contains = output
            .document()
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(function.id)
                    && relation.object == RelationEndpoint::Entity(argument.id)
            })
            .unwrap();
        assert_eq!(
            contains.evidence.source.as_ref().unwrap(),
            &definition(&output, argument).source
        );
        assert_eq!(contains.id, derive_relation_record_id(contains).unwrap());
    }
}

#[test]
fn body_edits_keep_header_identity_and_required_admission_is_atomic() {
    let source = "function stable -a argument\n echo one\nend\n# optional evidence\n";
    let output = analyze(source);
    let changed = analyze(&source.replace("echo one", "echo two and-more"));
    for (name, kind) in [
        ("stable", EntityKind::Function),
        ("argument", EntityKind::Parameter),
    ] {
        assert_eq!(
            entity(&output, name, kind).id,
            entity(&changed, name, kind).id
        );
    }
    let (_, required) = execute(source.as_bytes(), "utf-8", 4096).unwrap();
    assert!(required > 1);
    let (bounded, actual) = execute(source.as_bytes(), "utf-8", required).unwrap();
    assert_eq!(actual, required);
    entity(&bounded, "stable", EntityKind::Function);
    entity(&bounded, "argument", EntityKind::Parameter);
    let error = execute(source.as_bytes(), "utf-8", required - 1).unwrap_err();
    assert_eq!(
        error,
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1
        })
    );
}
