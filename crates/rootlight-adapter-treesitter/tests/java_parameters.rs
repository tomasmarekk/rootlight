//! Java formals use native method and constructor parameter fields.
//! Type names, body reads and lambda parameters do not establish named-method binders.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, EntityKind, ExtensionSupport, IrLimits, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate, SourceRef, SourceSpan,
    derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = include_str!("fixtures/java_parameters/AbsoluteValue.java");

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
    fs::write(directory.path().join("source.java"), raw).unwrap();
    let repository = derive_repository(b"java-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.java")).unwrap(),
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
            "java-source-contract",
            "1.0",
            content_hash(b"java-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("java").unwrap(),
        "java-static-contract",
        content_hash(b"java-contract-binary"),
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
        LanguageId::new("java").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"java-contract-build-context")),
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
fn original_method_parameter_has_exact_definition_and_owner() {
    let output = analyze(SOURCE);
    let owner = entity_at(&output, "getAbsValue", EntityKind::Method, 316, 11);
    let parameter = entity_at(&output, "number", EntityKind::Parameter, 332, 6);
    contains(&output, owner, parameter, 332, 338);
    assert!(!output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            && occurrence.source.span().start_byte() == 367
    }));
}

#[test]
fn method_and_constructor_formals_exclude_types_body_and_lambda_names() {
    let source = concat!(
        "class Container { Container(int seed) {} ",
        "void forms(@Mark final Box<String> value, int count[]) { ",
        "consume(value, count); Object callback = (int hidden) -> hidden; ",
        "String text = \"int phantom\"; /* int ghost */ } }",
    );
    let output = analyze(source);
    let constructor = entity_at(
        &output,
        "Container",
        EntityKind::Constructor,
        source.find("Container(int").unwrap(),
        9,
    );
    let method = entity_at(
        &output,
        "forms",
        EntityKind::Method,
        source.find("forms(").unwrap(),
        5,
    );
    for (name, owner) in [("seed", constructor), ("value", method), ("count", method)] {
        let start = source.find(name).unwrap();
        let parameter = entity_at(&output, name, EntityKind::Parameter, start, name.len());
        contains(&output, owner, parameter, start, start + name.len());
    }
    let parameters = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter)
        .map(|entity| entity.canonical_name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        parameters,
        std::collections::BTreeSet::from(["count", "seed", "value"])
    );
}

#[test]
fn encoded_parameter_spans_and_body_edit_identity_remain_exact() {
    let encoded = std::iter::once(0xfeff_u16)
        .chain(SOURCE.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let output = analyze_raw(&encoded, "utf-16le");
    let parameter = entity_at(&output, "number", EntityKind::Parameter, 666, 12);
    let owner = entity_at(&output, "getAbsValue", EntityKind::Method, 634, 22);
    contains(&output, owner, parameter, 666, 678);
    let before = analyze(SOURCE);
    let after = analyze(&SOURCE.replace("Math.abs(number)", "Math.abs(number) + 0"));
    let symbol = entity_at(&before, "number", EntityKind::Parameter, 332, 6);
    assert_eq!(
        symbol,
        entity_at(&after, "number", EntityKind::Parameter, 332, 6)
    );
}
