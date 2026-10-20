//! Pony analysis retains named types and directly owned members with physical evidence.
//! Imports, receiver capabilities and dispatch remain explicit semantic gaps.

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

const SOURCE: &str = include_str!("fixtures/pony/simple_server.pony");

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
    fs::write(directory.path().join("source.pony"), raw).unwrap();
    let repository = derive_repository(b"pony-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.pony")).unwrap(),
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
            "pony-source-contract",
            "1.0",
            content_hash(b"pony-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("pony").unwrap(),
        "pony-static-contract",
        content_hash(b"pony-contract-binary"),
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
        LanguageId::new("pony").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"pony-contract-build-context")),
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
fn original_class_and_methods_keep_exact_definition_and_owner_witnesses() {
    assert_eq!(SOURCE.len(), 1292);
    let output = analyze(SOURCE);
    let owner = entity_at(&output, "SimpleServer", EntityKind::Class, 413, 12);
    let connected = entity_at(&output, "connected", EntityKind::Method, 582, 9);
    let not_listening = entity_at(&output, "not_listening", EntityKind::Method, 666, 13);
    contains(&output, owner, connected, 582, 591);
    contains(&output, owner, not_listening, 666, 679);
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(output.document().skipped_regions.iter().any(
        |gap| gap.detail == "pony-import-type-composition-receiver-capability-and-dispatch-analysis-unavailable"
    ));
}

#[test]
fn native_roles_are_exact_and_data_or_anonymous_members_do_not_acquire_static_owners() {
    let source = concat!(
        "\"\"\"class Ghost fun phantom() => None\"\"\"\n// class Commented\n",
        "actor Worker\n  new create() => None\n  be ping() => None\n",
        "class Written\n  fun ref build(): Any =>\n    object\n      fun hidden() => None\n    end\n",
        "struct Record\nprimitive Token\ntrait Behavior\n  fun wanted()\ninterface Notice\n  be notify()\n"
    );
    let output = analyze(source);
    for (name, kind) in [
        ("Worker", EntityKind::Class),
        ("Written", EntityKind::Class),
        ("Record", EntityKind::Struct),
        ("Token", EntityKind::Class),
        ("Behavior", EntityKind::Trait),
        ("Notice", EntityKind::Interface),
        ("create", EntityKind::Constructor),
        ("ping", EntityKind::Method),
        ("build", EntityKind::Method),
    ] {
        let start = source.find(name).unwrap();
        entity_at(&output, name, kind, start, name.len());
    }
    for absent in ["Ghost", "phantom", "Commented", "hidden"] {
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
            .any(|gap| gap.detail == "pony-written-name-or-static-owner-proof-unavailable")
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
fn utf16_both_orders_preserve_definition_sources_and_contains() {
    let source = "// λ🙂 prefix\nclass Written\n  fun ref connected() => None\n";
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
            "Written",
            EntityKind::Class,
            start("Written"),
            "Written".len() * 2,
        );
        let member = entity_at(
            &output,
            "connected",
            EntityKind::Method,
            start("connected"),
            "connected".len() * 2,
        );
        contains(
            &output,
            owner,
            member,
            start("connected"),
            start("connected") + 18,
        );
        for occurrence in &output.document().occurrences {
            assert_eq!(occurrence.source.content_hash(), content_hash(&raw));
        }
    }
}

#[test]
fn body_edits_preserve_names_and_required_identity_admission_is_atomic() {
    let source = "class Stable\n  fun ref connected() => None\n// optional documentation\n";
    let original = analyze(source);
    let changed = analyze(&source.replace("=> None", "=> \"changed body\""));
    for (name, kind) in [
        ("Stable", EntityKind::Class),
        ("connected", EntityKind::Method),
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
        "connected",
        EntityKind::Method,
        source.find("connected").unwrap(),
        9,
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
