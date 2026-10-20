//! HCL analysis retains written fields, physical names and lexical containment.
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

const SOURCE: &str = include_str!("fixtures/hcl/versions.tf");

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
    fs::write(directory.path().join("source.hcl"), raw).unwrap();
    let repository = derive_repository(b"hcl-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.hcl")).unwrap(),
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
            "hcl-source-contract",
            "1.0",
            content_hash(b"hcl-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("hcl").unwrap(),
        "hcl-static-contract",
        content_hash(b"hcl-contract-binary"),
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
        LanguageId::new("hcl").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"hcl-contract-build-context")),
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
fn original_provider_properties_keep_all_authored_names_and_contains() {
    assert_eq!(SOURCE.len(), 946);
    let output = analyze(SOURCE);
    let hcloud = entity_at(&output, "hcloud", EntityKind::Property, 75, 6);
    let source = entity_at(&output, "source", EntityKind::Property, 93, 6);
    entity_at(&output, "version", EntityKind::Property, 132, 7);
    assert_eq!(&SOURCE.as_bytes()[75..81], b"hcloud");
    assert_eq!(&SOURCE.as_bytes()[93..99], b"source");
    assert_eq!(&SOURCE.as_bytes()[132..139], b"version");
    contains(&output, hcloud, source, 93, 99);
    assert!(!output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            && occurrence.source.span().start_byte() == 117
            && occurrence.source.span().end_byte() == 123
    }));
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
            .any(|gap| gap.detail
                == "hcl-expression-template-module-provider-and-runtime-evaluation-unavailable")
    );
}

#[test]
fn computed_quoted_generated_and_expression_values_do_not_donate_properties() {
    let source = concat!(
        "root = {\n first-name = 1\n nested = { child = 2 }\n",
        " (var.key) = { generated = 1 }\n \"quoted\" = { under_quote = 2 }\n}\n",
        "called = identity({ passed = 1 })\n",
        "made = { for key, value in var.items : key => { generated_for = value } }\n",
        "dynamic \"setting\" {\n for_each = var.items\n content { hidden_dynamic = 1 }\n}\n",
        "# phantom = 1\ntext = \"string_only = 2\"\n",
    );
    let output = analyze(source);
    for name in [
        "root",
        "first-name",
        "nested",
        "child",
        "called",
        "made",
        "text",
    ] {
        entity_at(
            &output,
            name,
            EntityKind::Property,
            source.find(name).unwrap(),
            name.len(),
        );
    }
    for name in [
        "generated",
        "quoted",
        "under_quote",
        "passed",
        "generated_for",
        "for_each",
        "hidden_dynamic",
        "phantom",
        "string_only",
    ] {
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "{name}"
        );
    }
    let parent = entity_at(
        &output,
        "nested",
        EntityKind::Property,
        source.find("nested").unwrap(),
        6,
    );
    let child_start = source.find("child").unwrap();
    let child = entity_at(&output, "child", EntityKind::Property, child_start, 5);
    contains(&output, parent, child, child_start, child_start + 5);
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "hcl-static-key-or-written-container-proof-unavailable")
    );
}

#[test]
fn utf16_both_orders_keep_original_physical_names_hashes_and_ownership() {
    let source = "# λ🙂 prefix\nstable = { child = 1 }\n";
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
        let start =
            |name: &str| 2 + 2 * source[..source.find(name).unwrap()].encode_utf16().count();
        let parent = entity_at(&output, "stable", EntityKind::Property, start("stable"), 12);
        let child = entity_at(&output, "child", EntityKind::Property, start("child"), 10);
        contains(&output, parent, child, start("child"), start("child") + 10);
        for name in ["stable", "child"] {
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
            assert_eq!(&raw[start(name)..start(name) + expected.len()], expected);
        }
        assert_eq!(output.document().files[0].content_hash, content_hash(&raw));
    }
}

#[test]
fn value_edits_preserve_identity_and_required_fact_admission_stays_exact() {
    let source = "stable = { child = 1 }\n# documentation\n";
    let original = analyze(source);
    let changed = analyze(&source.replace("= 1", "= 2"));
    for name in ["stable", "child"] {
        let start = source.find(name).unwrap();
        assert_eq!(
            entity_at(&original, name, EntityKind::Property, start, name.len()),
            entity_at(&changed, name, EntityKind::Property, start, name.len())
        );
    }
    let (_, required) = execute(source.as_bytes(), "utf-8", 4096).unwrap();
    assert!(required > 1);
    let (bounded, actual) = execute(source.as_bytes(), "utf-8", required).unwrap();
    assert_eq!(actual, required);
    entity_at(&bounded, "stable", EntityKind::Property, 0, 6);
    assert_eq!(
        execute(source.as_bytes(), "utf-8", required - 1).unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1,
        })
    );
    let malformed = analyze("survivor = 1\nbroken = {\n");
    assert!(!malformed.document().skipped_regions.is_empty());
    assert_ne!(
        malformed.report().coverage().status(),
        CoverageStatus::Complete
    );
}
