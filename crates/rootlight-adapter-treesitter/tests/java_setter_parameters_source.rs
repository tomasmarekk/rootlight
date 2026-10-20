//! Java setter RHS reads bind only their own explicitly declared formal.
//! Full compilation, field resolution and general method-body analysis are separate.

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
    AnalysisTier, BuildContextIdentity, EntityKind, ExtensionSupport, IrLimits, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate, SourceRef, SourceSpan,
    derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = include_str!("fixtures/java_setter_parameters/Product.java");

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
    execute_with_ir(raw, encoding, maximum, IrLimits::default())
}

fn execute_with_ir(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
    ir: IrLimits,
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
        ir,
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

fn resolved_formal_read(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    owner: rootlight_ids::SymbolId,
    parameter: rootlight_ids::SymbolId,
    start: usize,
    end: usize,
    raw: &[u8],
) {
    let document = output.document();
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(end).unwrap()
        })
        .collect();
    assert_eq!(sites.len(), 1, "exact authored formal read {start}..{end}");
    let site = sites[0];
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: parameter },
        "site={site:?}; diagnostics={:?}; gaps={:?}",
        document.diagnostics,
        document.skipped_regions
    );
    assert_eq!(site.enclosing, Some(owner));
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(
        site.syntactic_text_hash,
        content_hash(
            document
                .entities
                .iter()
                .find(|entity| entity.id == parameter)
                .unwrap()
                .canonical_name
                .as_bytes()
        )
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(parameter)
                && edge.evidence.source.as_ref() == Some(&site.source))
            .count(),
        1
    );
}

#[test]
fn original_product_setter_rhs_resolves_its_own_written_formal() {
    assert_eq!(SOURCE.len(), 513);
    let output = analyze(SOURCE);
    let method_start = SOURCE.find("setSku(").unwrap();
    let owner = entity_at(&output, "setSku", EntityKind::Method, method_start, 6);
    let parameter_start = SOURCE.find("setSku(String sku)").unwrap() + "setSku(String ".len();
    let parameter = entity_at(&output, "sku", EntityKind::Parameter, parameter_start, 3);
    contains(
        &output,
        owner,
        parameter,
        parameter_start,
        parameter_start + 3,
    );
    let rhs = SOURCE.find("= sku;").unwrap() + 2;
    resolved_formal_read(&output, owner, parameter, rhs, rhs + 3, SOURCE.as_bytes());
    let field = SOURCE.find("this.sku =").unwrap() + 5;
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.source.span().start_byte() == field as u64
                && site.target == OccurrenceTarget::Resolved { symbol: parameter })
    );
}

#[test]
fn same_spelling_in_sibling_methods_binds_only_each_current_formal() {
    let source = "class Sample { String field; void first(String value) { this.field = value; } void second(String value) { this.field = value; } }";
    let output = analyze(source);
    let mut parameters = Vec::new();
    for name in ["first", "second"] {
        let start = source.find(&format!("{name}(")).unwrap();
        let owner = entity_at(&output, name, EntityKind::Method, start, name.len());
        let formal_start = start + source[start..].find("String value").unwrap() + 7;
        let parameter = entity_at(&output, "value", EntityKind::Parameter, formal_start, 5);
        let rhs = start + source[start..].find("= value;").unwrap() + 2;
        resolved_formal_read(&output, owner, parameter, rhs, rhs + 5, source.as_bytes());
        parameters.push(parameter);
    }
    assert_ne!(parameters[0], parameters[1]);
}

#[test]
fn fields_locals_lambdas_nested_classes_and_other_methods_do_not_donate_formals() {
    for source in [
        "class Sample { String value; void set() { this.field = value; } }",
        "class Sample { void first(String value) {} void second() { this.field = value; } }",
        "class Sample { void set(String value) { String local = value; this.field = value; } }",
        "class Sample { void set(String value) { Runnable fn = () -> { this.field = value; }; } }",
        "class Sample { void set(String value) { class Nested { void apply() { this.field = value; } } } }",
        "class Sample { void set(String value, String value) { this.field = value; } }",
        "class Sample { void set(String value,) { this.field = value; } }",
        "class Sample { void set(String value) { this.field = value; this.other = value; } }",
    ] {
        let output = analyze(source);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "java.setter_parameter.reference"
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
    }
}

#[test]
fn utf16_setter_reads_after_unicode_trivia_keep_exact_physical_bytes() {
    let source =
        "// ∆\r\nclass Sample { String field; void set(String value) { this.field = value; } }\r\n";
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            raw.extend(if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let output = analyze_raw(&raw, if little { "utf-16le" } else { "utf-16be" });
        let physical = |offset| 2 + source[..offset].encode_utf16().count() * 2;
        let method_start = source.find("set(").unwrap();
        let owner = entity_at(
            &output,
            "set",
            EntityKind::Method,
            physical(method_start),
            6,
        );
        let parameter_start = source.find("String value").unwrap() + 7;
        let parameter = entity_at(
            &output,
            "value",
            EntityKind::Parameter,
            physical(parameter_start),
            10,
        );
        let rhs = source.find("= value;").unwrap() + 2;
        resolved_formal_read(
            &output,
            owner,
            parameter,
            physical(rhs),
            physical(rhs) + 10,
            &raw,
        );
    }
}

#[test]
fn body_edit_keeps_formal_identity_and_required_context_admission_is_atomic() {
    let source = "class Sample { String field; void set(String value) { this.field = value; } }";
    let original = analyze(source);
    let changed = analyze(&source.replace("this.field", "this.other"));
    let start = source.find("String value").unwrap() + 7;
    assert_eq!(
        entity_at(&original, "value", EntityKind::Parameter, start, 5),
        entity_at(&changed, "value", EntityKind::Parameter, start, 5)
    );
    assert!(original.document().skipped_regions.iter().any(|gap| {
        gap.domain == rootlight_ir::FactDomain::Relations
            && gap.detail == "java-formal-binding-proof-is-limited-to-single-assignment-setters"
    }));
    let (_, required) = execute(source.as_bytes(), "utf-8", 4096).unwrap();
    assert!(required > 1);
    let (bounded, actual) = execute(source.as_bytes(), "utf-8", required).unwrap();
    assert_eq!(actual, required);
    entity_at(&bounded, "value", EntityKind::Parameter, start, 5);
    assert_eq!(
        execute(source.as_bytes(), "utf-8", required - 1).unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1
        })
    );
}

#[test]
fn resolved_reads_and_scope_limits_fit_exact_record_admission() {
    let source = "class Sample { String field; void set(String value) { this.field = value; } }";
    for relation_bound in [true, false] {
        let selected = |maximum| {
            let mut limits = IrLimits::default();
            if relation_bound {
                limits.max_relations = maximum;
            } else {
                limits.max_total_records = maximum;
            }
            limits
        };
        let refusal = execute_with_ir(source.as_bytes(), "utf-8", 4096, selected(1));
        let Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::Records,
            observed,
            limit: 1,
        })) = refusal
        else {
            panic!("the original source must expose its complete record demand: {refusal:?}")
        };
        assert!(observed > 1);
        for exact in [true, false] {
            let maximum = observed - usize::from(!exact);
            let result = execute_with_ir(source.as_bytes(), "utf-8", 4096, selected(maximum));
            if exact {
                let output = result.unwrap().0;
                let owner = entity_at(
                    &output,
                    "set",
                    EntityKind::Method,
                    source.find("set(").unwrap(),
                    3,
                );
                let start = source.find("String value").unwrap() + 7;
                let parameter = entity_at(&output, "value", EntityKind::Parameter, start, 5);
                let rhs = source.find("= value;").unwrap() + 2;
                resolved_formal_read(&output, owner, parameter, rhs, rhs + 5, source.as_bytes());
                assert!(
                    output
                        .document()
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.detail
                            == "java-formal-binding-proof-is-limited-to-single-assignment-setters")
                );
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    AdapterError::Sink(SinkError::StreamLimit {
                        resource: ResourceKind::Records,
                        observed,
                        limit: maximum,
                    })
                );
            }
        }
    }
}
