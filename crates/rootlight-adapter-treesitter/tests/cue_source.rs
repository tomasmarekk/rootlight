//! CUE analysis retains written fields, physical names and lexical containment.
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

const SOURCE: &str = include_str!("fixtures/cue/environment.cue");

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
    fs::write(directory.path().join("source.cue"), raw).unwrap();
    let repository = derive_repository(b"cue-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.cue")).unwrap(),
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
            "cue-source-contract",
            "1.0",
            content_hash(b"cue-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("cue").unwrap(),
        "cue-static-contract",
        content_hash(b"cue-contract-binary"),
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
        LanguageId::new("cue").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"cue-contract-build-context")),
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

fn contains(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    parent: &rootlight_ir::EntityRecord,
    child: &rootlight_ir::EntityRecord,
) {
    let relations = output
        .document()
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(parent.id)
                && relation.object == RelationEndpoint::Entity(child.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        relations.len(),
        1,
        "{} -> {}",
        parent.canonical_name,
        child.canonical_name
    );
    let source = relations[0].evidence.source.as_ref().unwrap();
    assert_eq!(source, child.evidence.source.as_ref().unwrap());
    let name = &definition(output, child).source;
    assert_eq!(source.repository(), name.repository());
    assert_eq!(source.generation(), name.generation());
    assert_eq!(source.content_hash(), name.content_hash());
    assert_eq!(source.span().file(), name.span().file());
    assert!(source.span().start_byte() <= name.span().start_byte());
    assert!(source.span().end_byte() >= name.span().end_byte());
    assert_eq!(
        relations[0].id,
        derive_relation_record_id(relations[0]).unwrap()
    );
}

#[test]
fn original_source_fields_and_contains_match_exact_finite_witnesses() {
    let output = analyze(SOURCE);
    for (name, start, end) in [
        ("UserDefined", 58, 69),
        ("buildkit", 93, 101),
        ("address", 108, 115),
        ("hosts", 152, 157),
    ] {
        let record = entity(&output, name, EntityKind::Field);
        let occurrence = definition(&output, record);
        assert_eq!(occurrence.source.span().start_byte(), start);
        assert_eq!(occurrence.source.span().end_byte(), end);
        assert_eq!(
            &SOURCE[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
            name
        );
    }
    for (parent, child) in [
        ("UserDefined", "buildkit"),
        ("buildkit", "address"),
        ("UserDefined", "hosts"),
    ] {
        contains(
            &output,
            entity(&output, parent, EntityKind::Field),
            entity(&output, child, EntityKind::Field),
        );
    }
    assert!(!output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            && occurrence.source.span().start_byte() == 192
            && occurrence.source.span().end_byte() == 201
    }));
    assert_eq!(
        SOURCE
            .match_indices("UserDefined")
            .map(|(start, _)| start)
            .collect::<Vec<_>>(),
        [58]
    );
    assert_eq!(
        SOURCE
            .matches("UserDefined__absent_source_control__")
            .count(),
        0
    );
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
                == "cue-constraint-evaluation-comprehension-and-import-semantics-unavailable")
    );
    contains(
        &output,
        entity(&output, "hosts", EntityKind::Field),
        entity(&output, "snapshot.debian.org", EntityKind::Field),
    );
    contains(
        &output,
        entity(&output, "snapshot.debian.org", EntityKind::Field),
        entity(&output, "ip", EntityKind::Field),
    );
}

#[test]
fn static_definition_optional_required_and_chained_labels_keep_written_names() {
    let source = "package sample\n#Shape: {optional?: string, required!: int}\na: b: c: string\n\"literal.name\": true\n";
    let output = analyze(source);
    for name in [
        "#Shape",
        "optional",
        "required",
        "a",
        "b",
        "c",
        "literal.name",
    ] {
        let record = entity(&output, name, EntityKind::Field);
        let occurrence = definition(&output, record);
        let span = occurrence.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(name.as_bytes())
        );
    }
    for (parent, child) in [
        ("#Shape", "optional"),
        ("#Shape", "required"),
        ("a", "b"),
        ("b", "c"),
    ] {
        contains(
            &output,
            entity(&output, parent, EntityKind::Field),
            entity(&output, child, EntityKind::Field),
        );
    }
    assert!(!output.document().entities.iter().any(|record| matches!(
        record.canonical_name.as_str(),
        "optional?" | "required!" | "Shape" | "\"literal.name\""
    )));
}

#[test]
fn dynamic_alias_comprehension_and_expression_text_do_not_fabricate_fields() {
    let source = "package sample\nimport \"example.invalid/schema\"\nname: \"computed\"\nstable: schema.#Imported\n(name): {hidden: 1}\n[string]: {patterned: 1}\nfor key, value in {a: 1} {\"\\(key)\": {generated: value}}\nalias=written: int\n\"escaped\\u0061\": int\n// commented: {phantom: 1}\ntext: \"string_only: 1\"\n";
    let output = analyze(source);
    entity(&output, "stable", EntityKind::Field);
    for absent in [
        "computed",
        "Imported",
        "hidden",
        "patterned",
        "key",
        "value",
        "generated",
        "alias",
        "written",
        "escapeda",
        "commented",
        "phantom",
        "string_only",
    ] {
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|record| record.canonical_name == absent),
            "{absent}: {:?}",
            output.document().entities
        );
    }
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "cue-static-label-or-lexical-container-proof-unavailable")
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
fn utf16_labels_and_lexical_contains_preserve_original_physical_evidence() {
    let source = "// λ🙂 prefix\n#Owner: {naïve?: string, \"original.label\": int}\n";
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
        for name in ["#Owner", "naïve", "original.label"] {
            let record = entity(&output, name, EntityKind::Field);
            let occurrence = definition(&output, record);
            let start = source.find(name).unwrap();
            let physical_start = 2 + source[..start].encode_utf16().count() * 2;
            let written = name
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
                occurrence.source.span().start_byte(),
                u64::try_from(physical_start).unwrap()
            );
            assert_eq!(
                occurrence.source.span().end_byte(),
                u64::try_from(physical_start + written.len()).unwrap()
            );
            assert_eq!(
                &raw[physical_start..physical_start + written.len()],
                written
            );
            assert_eq!(occurrence.source.content_hash(), content_hash(&raw));
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(name.as_bytes())
            );
        }
        for child in ["naïve", "original.label"] {
            contains(
                &output,
                entity(&output, "#Owner", EntityKind::Field),
                entity(&output, child, EntityKind::Field),
            );
        }
    }
}

#[test]
fn value_edits_preserve_written_field_identity_and_admission_is_atomic() {
    let source = "package sample\nparent: {child: 1}\n";
    let output = analyze(source);
    let changed = analyze(&source.replace("child: 1", "child: 200"));
    for name in ["parent", "child"] {
        assert_eq!(
            entity(&output, name, EntityKind::Field).id,
            entity(&changed, name, EntityKind::Field).id
        );
    }
    let (_, required) = execute(source.as_bytes(), "utf-8", 4096).unwrap();
    assert!(required > 1);
    let (bounded, observed) = execute(source.as_bytes(), "utf-8", required).unwrap();
    assert_eq!(observed, required);
    contains(
        &bounded,
        entity(&bounded, "parent", EntityKind::Field),
        entity(&bounded, "child", EntityKind::Field),
    );
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
fn malformed_field_does_not_supply_a_name_or_poison_an_unrelated_owner() {
    let source = "package sample\nvalid: {keep: 1}\nbroken: {ghost:\n";
    let output = analyze(source);
    contains(
        &output,
        entity(&output, "valid", EntityKind::Field),
        entity(&output, "keep", EntityKind::Field),
    );
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|record| matches!(record.canonical_name.as_str(), "broken" | "ghost"))
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("cue-") || gap.detail.contains("parse"))
    );
}
