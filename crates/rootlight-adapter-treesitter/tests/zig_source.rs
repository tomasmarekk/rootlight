//! Zig analysis retains written functions and const/var binders with physical evidence.
//! Imports, comptime evaluation, type inference and resolved calls remain explicit gaps.

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

const SOURCE: &str = include_str!("fixtures/zig/pipe.zig");

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
    fs::write(directory.path().join("source.zig"), raw).unwrap();
    let repository = derive_repository(b"zig-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.zig")).unwrap(),
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
            "zig-source-contract",
            "1.0",
            content_hash(b"zig-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("zig").unwrap(),
        "zig-static-contract",
        content_hash(b"zig-contract-binary"),
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
        LanguageId::new("zig").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"zig-contract-build-context")),
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
fn original_function_and_branch_locals_preserve_all_finite_written_witnesses() {
    assert_eq!(SOURCE.len(), 772);
    let output = analyze(SOURCE);
    let owner = entity_at(&output, "pipe", EntityKind::Function, 298, 4);
    let read = entity_at(&output, "read", EntityKind::Variable, 452, 4);
    entity_at(&output, "write", EntityKind::Variable, 502, 5);
    contains(&output, owner, read, 452, 456);
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(read)
                && relation.object == RelationEndpoint::Entity(owner))
    );
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                && occurrence.source.span().start_byte() == 586
                && occurrence.source.span().end_byte() == 590)
    );
    assert_eq!(
        SOURCE
            .match_indices("read")
            .map(|(start, _)| start)
            .collect::<Vec<_>>(),
        vec![452, 586, 738]
    );
    assert!(!SOURCE.contains("read__rootlight_literal_absent__"));
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
                == "zig-import-type-comptime-receiver-and-call-analysis-unavailable")
    );
}

#[test]
fn assignments_uses_strings_quoted_and_evaluated_contexts_do_not_invent_definitions() {
    let source = concat!(
        "// fn commented() void {}\nconst text = \"fn phantom() void {}\";\n",
        "pub fn written() void { var local: u8 = 1; local = 2; _ = &local; }\n",
        "comptime { const generated = 1; _ = generated; }\n",
        "const Container = struct { pub fn member() void {} };\n",
        "fn @\"quoted name\"() void { const hidden = 1; _ = hidden; }\n"
    );
    let output = analyze(source);
    let owner_start = source.find("written").unwrap();
    let local_start = source.find("local").unwrap();
    let owner = entity_at(&output, "written", EntityKind::Function, owner_start, 7);
    let local = entity_at(&output, "local", EntityKind::Variable, local_start, 5);
    contains(&output, owner, local, local_start, local_start + 5);
    let definitions = output
        .document()
        .occurrences
        .iter()
        .filter(|row| {
            row.role == OccurrenceRole::Definition
                && row.source.span().start_byte() > u64::try_from(local_start).unwrap()
                && row.source.span().start_byte()
                    < u64::try_from(source.find("comptime").unwrap()).unwrap()
        })
        .count();
    assert_eq!(
        definitions, 0,
        "Assignments and address uses are not declarations"
    );
    for absent in [
        "commented",
        "phantom",
        "generated",
        "member",
        "quoted name",
        "hidden",
    ] {
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == absent),
            "{absent}"
        );
    }
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "zig-static-binder-or-written-owner-proof-unavailable")
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
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|row| row.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn utf16_both_orders_keep_physical_name_ranges_content_identity_and_contains() {
    let source =
        "// λ🙂 prefix\npub fn written() void { if (true) { var local: u8 = 1; _ = &local; } }\n";
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
        let owner = entity_at(
            &output,
            "written",
            EntityKind::Function,
            start("written"),
            14,
        );
        let local = entity_at(&output, "local", EntityKind::Variable, start("local"), 10);
        contains(&output, owner, local, start("local"), start("local") + 10);
        for name in ["written", "local"] {
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
        for occurrence in &output.document().occurrences {
            assert_eq!(occurrence.source.content_hash(), content_hash(&raw));
        }
    }
}

#[test]
fn body_identity_required_capture_capacity_and_parser_gaps_remain_explicit() {
    let source =
        "pub fn stable() void { var local: u8 = 1; _ = &local; }\n// optional documentation\n";
    let original = analyze(source);
    let changed = analyze(&source.replace("= 1", "= 2"));
    for (name, kind) in [
        ("stable", EntityKind::Function),
        ("local", EntityKind::Variable),
    ] {
        let start = source.find(name).unwrap();
        assert_eq!(
            entity_at(&original, name, kind, start, name.len()),
            entity_at(&changed, name, kind, start, name.len())
        );
    }
    let (_, required) = execute(source.as_bytes(), "utf-8", 4096).unwrap();
    assert!(required > 1);
    let (bounded, actual) = execute(source.as_bytes(), "utf-8", required).unwrap();
    assert_eq!(required, actual);
    entity_at(
        &bounded,
        "stable",
        EntityKind::Function,
        source.find("stable").unwrap(),
        6,
    );
    assert_eq!(
        execute(source.as_bytes(), "utf-8", required - 1).unwrap_err(),
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            observed: required,
            limit: required - 1
        })
    );
    let malformed = "pub fn survivor() void {}\npub fn broken( {\n";
    let output = analyze(malformed);
    entity_at(&output, "survivor", EntityKind::Function, 7, 8);
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "broken")
    );
    assert!(!output.document().skipped_regions.is_empty());
    assert_ne!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
}
