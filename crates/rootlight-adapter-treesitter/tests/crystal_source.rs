//! Crystal analysis retains named types and directly owned members with physical evidence.
//! Requires, type inference, macro expansion and dispatch remain explicit gaps.

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

const SOURCE: &str = include_str!("fixtures/crystal/null_log_handler.cr");

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
    fs::write(directory.path().join("source.cr"), raw).unwrap();
    let repository = derive_repository(b"crystal-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.cr")).unwrap(),
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
            "crystal-source-contract",
            "1.0",
            content_hash(b"crystal-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("crystal").unwrap(),
        "crystal-static-contract",
        content_hash(b"crystal-contract-binary"),
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
        LanguageId::new("crystal").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"crystal-contract-build-context")),
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
    assert_eq!(SOURCE.len(), 313);
    let output = analyze(SOURCE);
    let owner = entity_at(&output, "NullLogHandler", EntityKind::Class, 144, 14);
    let call = entity_at(&output, "call", EntityKind::Method, 191, 4);
    let write = entity_at(&output, "write", EntityKind::Method, 271, 5);
    contains(&output, owner, call, 191, 195);
    contains(&output, owner, write, 271, 276);
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(output.document().skipped_regions.iter().any(|gap| gap.detail == "crystal-require-type-inference-macro-evaluation-and-dispatch-analysis-unavailable"));
}

#[test]
fn only_written_lexical_owners_admit_methods_and_dynamic_contexts_remain_gaps() {
    let source = concat!(
        "# class Commented\n\"class Ghost; def phantom; end; end\"\n",
        "module Outer\nclass Written\ndef ready?\ntrue\nend\nend\n",
        "struct Record\ndef write!(value)\nvalue\nend\nend\nenum Choice\nFirst\nend\nend\n",
        "class Generic(T)\ndef hidden\nend\nend\n",
        "class Qualified::Owner\ndef qualified\nend\nend\n",
        "if flag\nclass Conditional\ndef conditional\nend\nend\nend\n",
        "macro template\nclass Generated\ndef generated\nend\nend\nend\n",
        "class Static\ndef self.singleton\nend\nend\n"
    );
    let output = analyze(source);
    for (name, kind) in [
        ("Outer", EntityKind::Module),
        ("Written", EntityKind::Class),
        ("ready?", EntityKind::Method),
        ("Record", EntityKind::Struct),
        ("write!", EntityKind::Method),
        ("Choice", EntityKind::Enum),
        ("Static", EntityKind::Class),
    ] {
        let start = source.find(name).unwrap();
        entity_at(&output, name, kind, start, name.len());
    }
    for absent in [
        "Commented",
        "Ghost",
        "phantom",
        "Generic",
        "hidden",
        "Qualified",
        "qualified",
        "Conditional",
        "conditional",
        "Generated",
        "generated",
        "singleton",
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
    let id = |name: &str| {
        output
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap()
            .id
    };
    contains(
        &output,
        id("Outer"),
        id("Written"),
        source.find("Written").unwrap(),
        source.find("Written").unwrap() + 7,
    );
    contains(
        &output,
        id("Written"),
        id("ready?"),
        source.find("ready?").unwrap(),
        source.find("ready?").unwrap() + 6,
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "crystal-written-name-or-static-owner-proof-unavailable")
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
fn utf16_both_orders_preserve_physical_definition_sources_and_contains() {
    let source =
        "# λ🙂 prefix\nmodule Outer\nclass Written\ndef connected(value)\nvalue\nend\nend\nend\n";
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
        let owner = entity_at(&output, "Written", EntityKind::Class, start("Written"), 14);
        let method = entity_at(
            &output,
            "connected",
            EntityKind::Method,
            start("connected"),
            18,
        );
        contains(
            &output,
            owner,
            method,
            start("connected"),
            start("connected") + 18,
        );
        let selected = &raw[start("connected")..start("connected") + 18];
        let expected: Vec<u8> = "connected"
            .encode_utf16()
            .flat_map(|unit| {
                if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect();
        assert_eq!(selected, expected);
        for occurrence in &output.document().occurrences {
            assert_eq!(occurrence.source.content_hash(), content_hash(&raw));
        }
    }
}

#[test]
fn body_edits_preserve_names_and_required_identity_admission_is_atomic() {
    let source = "class Stable\ndef connected\n42\nend\nend\n# optional documentation\n";
    let original = analyze(source);
    let changed = analyze(&source.replace("42", "\"changed body\""));
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
