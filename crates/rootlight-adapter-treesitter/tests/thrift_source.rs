//! Thrift contracts separate exact written IDL structure from codegen and validation.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, GenerationBoundSnapshot, LanguageId,
    MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, EntityKind, ExtensionSupport, FactRef, IrLimits,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate,
    SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = "// 🌐 written source\r\ninclude \"external.thrift\"\r\nnamespace java sample.api\r\nnamespace py sample.api\r\ntypedef i64 Key\r\nenum Status { READY = 0, DONE = 1 }\r\nstruct Item { 1: Key id; 2: Status status; }\r\nunion Choice { 1: Item item; }\r\nexception Problem { 1: string reason; }\r\nconst i32 LIMIT = 3\r\nservice Base { void ping(); }\r\nservice Store extends Base { Item fetch(1: Key key) throws (1: Problem problem); }\r\n";

fn analyze_at(path: &str, text: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    let directory = tempdir().unwrap();
    let file = directory.path().join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
    let repository = derive_repository(b"thrift-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 1024 * 1024)
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = TreeSitterProvider::new(
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
    .unwrap();
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(provider),
        ProducerIdentity::new(
            "thrift-source-contract",
            "1.0",
            content_hash(b"thrift-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("thrift").unwrap(),
        "thrift-static-contract",
        content_hash(b"thrift-contract-binary"),
    )
    .unwrap();
    let batch = BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        4096,
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
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("thrift").unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"thrift-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )
    .unwrap();
    let document = output.document();
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].language, "thrift");
    assert_eq!(
        document.files[0].content_hash,
        content_hash(text.as_bytes())
    );
    assert_eq!(
        document.files[0].byte_length,
        u64::try_from(text.len()).unwrap()
    );
    for occurrence in &document.occurrences {
        assert_eq!(occurrence.source.repository(), repository);
        assert_eq!(occurrence.source.generation(), source.generation());
        assert_eq!(occurrence.source.span().file(), source.span().file());
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(text.as_bytes())
        );
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(spelling(text, &occurrence.source).as_bytes())
        );
    }
    for gap in &document.skipped_regions {
        assert_eq!(gap.source.content_hash(), content_hash(text.as_bytes()));
        let _ = spelling(text, &gap.source);
    }
    output
}

fn spelling<'a>(text: &'a str, source: &SourceRef) -> &'a str {
    text.get(
        usize::try_from(source.span().start_byte()).unwrap()
            ..usize::try_from(source.span().end_byte()).unwrap(),
    )
    .unwrap()
}

fn entity<'a>(
    output: &'a rootlight_adapter_sdk::AnalysisOutput,
    name: &str,
    kind: EntityKind,
) -> &'a rootlight_ir::EntityRecord {
    let mut found = output
        .document()
        .entities
        .iter()
        .filter(|record| record.canonical_name == name && record.kind == kind);
    let result = found
        .next()
        .unwrap_or_else(|| panic!("missing {kind:?} {name}"));
    assert!(found.next().is_none(), "ambiguous fixture {name}");
    result
}

fn definition(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    text: &str,
    name: &str,
    kind: EntityKind,
    marker: &str,
) {
    let owner = entity(output, name, kind);
    let found = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: owner.id }
        })
        .collect::<Vec<_>>();
    assert_eq!(found.len(), 1, "{name}");
    let start = text.find(marker).unwrap() + marker.find(name).unwrap();
    assert_eq!(
        found[0].source.span().start_byte(),
        u64::try_from(start).unwrap()
    );
    assert_eq!(
        found[0].source.span().end_byte(),
        u64::try_from(start + name.len()).unwrap()
    );
    assert_eq!(spelling(text, &found[0].source), name);
}

#[test]
fn ordinary_idl_definitions_have_exact_names_and_lexical_owners() {
    let output = analyze_at("schemas/types.thrift", SOURCE);
    for (name, kind, marker) in [
        (
            "java sample.api",
            EntityKind::Namespace,
            "namespace java sample.api",
        ),
        (
            "py sample.api",
            EntityKind::Namespace,
            "namespace py sample.api",
        ),
        ("Key", EntityKind::TypeAlias, "typedef i64 Key"),
        ("Status", EntityKind::Enum, "enum Status"),
        ("READY", EntityKind::Constant, "READY = 0"),
        ("Item", EntityKind::Struct, "struct Item"),
        ("Choice", EntityKind::Union, "union Choice"),
        ("Problem", EntityKind::Struct, "exception Problem"),
        ("LIMIT", EntityKind::Constant, "const i32 LIMIT"),
        ("Store", EntityKind::Interface, "service Store"),
        ("fetch", EntityKind::Method, "Item fetch"),
        ("key", EntityKind::Parameter, "Key key"),
        ("problem", EntityKind::Parameter, "Problem problem"),
        ("id", EntityKind::Field, "Key id"),
    ] {
        definition(&output, SOURCE, name, kind, marker);
    }
    for (parent_name, parent_kind, child_name, child_kind) in [
        (
            "schemas/types.thrift",
            EntityKind::Module,
            "Item",
            EntityKind::Struct,
        ),
        ("Item", EntityKind::Struct, "id", EntityKind::Field),
        ("Status", EntityKind::Enum, "READY", EntityKind::Constant),
        ("Store", EntityKind::Interface, "fetch", EntityKind::Method),
        ("fetch", EntityKind::Method, "key", EntityKind::Parameter),
        (
            "fetch",
            EntityKind::Method,
            "problem",
            EntityKind::Parameter,
        ),
    ] {
        let parent = entity(&output, parent_name, parent_kind);
        let child = entity(&output, child_name, child_kind);
        assert_eq!(
            child.container,
            Some(rootlight_ir::ContainerRef::Entity(parent.id))
        );
        assert!(
            output
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(parent.id)
                    && relation.object == RelationEndpoint::Entity(child.id))
        );
    }
    assert_ne!(
        entity(&output, "java sample.api", EntityKind::Namespace).id,
        entity(&output, "py sample.api", EntityKind::Namespace).id
    );
}

#[test]
fn actual_file_module_and_codegen_namespaces_do_not_prefix_local_type_targets() {
    let output = analyze_at("schemas/types.thrift", SOURCE);
    assert_eq!(
        entity(&output, "Item", EntityKind::Struct).qualified_name,
        "schemas/types.thrift::Item"
    );
    let uses = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::TypeUse)
        .collect::<Vec<_>>();
    assert_eq!(uses.len(), 7);
    for (name, kind, count) in [
        ("Key", EntityKind::TypeAlias, 2),
        ("Status", EntityKind::Enum, 1),
        ("Item", EntityKind::Struct, 2),
        ("Base", EntityKind::Interface, 1),
        ("Problem", EntityKind::Struct, 1),
    ] {
        let target = entity(&output, name, kind);
        let found = uses
            .iter()
            .filter(|occurrence| spelling(SOURCE, &occurrence.source) == name)
            .collect::<Vec<_>>();
        assert_eq!(found.len(), count, "{name}");
        for occurrence in found {
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol: target.id }
            );
            assert!(
                output
                    .document()
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::UsesType
                        && relation.subject == RelationEndpoint::Occurrence(occurrence.id)
                        && relation.object == RelationEndpoint::Entity(target.id)
                        && relation.evidence.source.as_ref() == Some(&occurrence.source))
            );
        }
    }
}

fn field_type_edge(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    source: &str,
    field_name: &str,
    target: &rootlight_ir::EntityRecord,
    marker: &str,
) {
    let field = entity(output, field_name, EntityKind::Field);
    let offset = source.find(marker).unwrap() + marker.find(&target.canonical_name).unwrap();
    let occurrence = output
        .document()
        .occurrences
        .iter()
        .find(|row| {
            row.role == OccurrenceRole::TypeUse
                && row.source.span().start_byte() == u64::try_from(offset).unwrap()
        })
        .unwrap();
    assert_eq!(spelling(source, &occurrence.source), target.canonical_name);
    assert_eq!(occurrence.enclosing, Some(field.id));
    assert_eq!(
        occurrence.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert!(output.document().relations.iter().any(|row| {
        row.predicate == RelationPredicate::UsesType
            && row.subject == RelationEndpoint::Occurrence(occurrence.id)
            && row.object == RelationEndpoint::Entity(target.id)
            && row.evidence.source.as_ref() == Some(&occurrence.source)
    }));
    let relations = output
        .document()
        .relations
        .iter()
        .filter(|row| {
            row.predicate == RelationPredicate::UsesType
                && row.subject == RelationEndpoint::Entity(field.id)
                && row.object == RelationEndpoint::Entity(target.id)
                && row.evidence.source.as_ref() == Some(&occurrence.source)
        })
        .collect::<Vec<_>>();
    assert_eq!(relations.len(), 1, "{marker}");
    assert_eq!(relations[0].provenance, occurrence.provenance);
    assert_eq!(relations[0].confidence, occurrence.confidence);
    let mut expected_derivation = [
        FactRef::Entity(field.id),
        FactRef::Fact(occurrence.id),
        FactRef::Entity(target.id),
    ];
    expected_derivation.sort_unstable();
    assert_eq!(relations[0].evidence.derivation, expected_derivation);
    assert_eq!(
        relations[0].id,
        rootlight_ir::derive_relation_record_id(relations[0]).unwrap()
    );
}

#[test]
fn field_type_relations_keep_exact_named_and_container_type_evidence() {
    let source = "enum Flag { ON = 1 }\ntypedef i64 Key\nstruct Record { 1: Flag status; 2: list<Flag> flags; 3: map<Key, Flag> mapping; }\n";
    let output = analyze_at("schema.thrift", source);
    let flag = entity(&output, "Flag", EntityKind::Enum);
    let key = entity(&output, "Key", EntityKind::TypeAlias);
    for (field, target, marker) in [
        ("status", flag, "Flag status"),
        ("flags", flag, "list<Flag> flags"),
        ("mapping", key, "map<Key, Flag> mapping"),
        ("mapping", flag, "map<Key, Flag> mapping"),
    ] {
        field_type_edge(&output, source, field, target, marker);
    }
    assert_eq!(
        output
            .document()
            .relations
            .iter()
            .filter(|row| row.predicate == RelationPredicate::UsesType
                && matches!(row.subject, RelationEndpoint::Entity(_)))
            .count(),
        4
    );
}

#[test]
fn field_type_relations_do_not_guess_ambiguous_foreign_or_signature_owners() {
    for source in [
        "struct Item {} typedef i64 Item struct Host { 1: Item value; }",
        "struct Item {} struct Host { 1: item value; }",
        "struct Item {} struct Host { 1: external.Item value; }",
        "struct Item {} service Api { Item fetch(1: Item input); }",
    ] {
        let output = analyze_at("schema.thrift", source);
        assert!(
            !output.document().relations.iter().any(|row| {
                row.predicate == RelationPredicate::UsesType
                    && matches!(row.subject, RelationEndpoint::Entity(_))
            }),
            "{source}"
        );
        if source.contains("service Api") {
            assert_eq!(
                output
                    .document()
                    .relations
                    .iter()
                    .filter(|row| row.predicate == RelationPredicate::UsesType
                        && matches!(row.subject, RelationEndpoint::Occurrence(_)))
                    .count(),
                2
            );
        }
    }
}

#[test]
fn documentary_tokens_preserve_raw_spans_without_becoming_unavailable_names() {
    let source = "// Item stays documentary\ninclude \"remote.thrift\"\nconst string NOTE = \"Item stays text\"\nstruct Host { 1: Missing value; }\n";
    let output = analyze_at("schema.thrift", source);
    for (role, label, written) in [
        (
            OccurrenceRole::Documentation,
            "thrift.comment.comment",
            "// Item stays documentary",
        ),
        (
            OccurrenceRole::StringEvidence,
            "thrift.string.string",
            "\"Item stays text\"",
        ),
    ] {
        let rows = output
            .document()
            .occurrences
            .iter()
            .filter(|row| row.role == role && spelling(source, &row.source) == written)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "{written}");
        assert_eq!(rows[0].syntax_kind, label);
        assert_eq!(
            rows[0].source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(
            rows[0].source.span().start_byte(),
            u64::try_from(source.find(written).unwrap()).unwrap()
        );
    }
    assert!(
        !output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "thrift-reference-name-unavailable")
    );
    for detail in [
        "thrift-written-type-target-unavailable",
        "thrift-include-target-unavailable",
    ] {
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == detail),
            "{detail}"
        );
    }
}

#[test]
fn includes_and_qualified_foreign_types_remain_unresolved_exact_written_evidence() {
    let text = "include 'external.thrift'\nnamespace java external\nstruct Item {} struct Holder { 1: external.Item value; }";
    let output = analyze_at("schema.thrift", text);
    for (label, written, gap) in [
        (
            "thrift.type.reference",
            "external.Item",
            "thrift-written-type-target-unavailable",
        ),
        (
            "thrift.written_include.import",
            "'external.thrift'",
            "thrift-include-target-unavailable",
        ),
    ] {
        let found = output
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.syntax_kind == label)
            .collect::<Vec<_>>();
        assert_eq!(found.len(), 1);
        assert_eq!(spelling(text, &found[0].source), written);
        assert!(matches!(
            found[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|region| region.detail == gap)
        );
    }
}

#[test]
fn ambiguous_type_or_service_names_never_select_an_arbitrary_authored_target() {
    for text in [
        "struct Item {} typedef i64 Item struct Holder { 1: Item value; }",
        "struct Item {} service Item {} service Derived extends Item {}",
    ] {
        let output = analyze_at("schema.thrift", text);
        let uses = output
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::TypeUse)
            .collect::<Vec<_>>();
        assert_eq!(uses.len(), 1);
        assert!(matches!(
            uses[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::UsesType)
        );
    }
}

#[test]
fn absent_uuid_primitive_never_resolves_as_a_user_type() {
    let text = "struct uuid {} struct Holder { 1: uuid value; }";
    let output = analyze_at("schema.thrift", text);
    let uses = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "thrift.uuid_type.reference")
        .collect::<Vec<_>>();
    assert_eq!(uses.len(), 1);
    assert_eq!(spelling(text, &uses[0].source), "uuid");
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.detail == "thrift-uuid-primitive-grammar-unavailable"
                    && spelling(text, &gap.source) == "uuid"
            )
    );
}

#[test]
fn unsupported_owners_and_recovery_keep_ranged_gaps_without_leaked_members() {
    for text in [
        "interaction Chat { void hidden(); }",
        "senum Legacy { 'value' }",
        "struct { 1: string hidden; }",
        "struct Outer /* trivia */ . Inner { 1: string hidden; }",
    ] {
        let output = analyze_at("schema.thrift", text);
        assert!(!output.document().entities.iter().any(|entity| matches!(
            entity.canonical_name.as_str(),
            "Chat" | "Legacy" | "hidden" | "Outer" | "Inner"
        )));
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(
                    |gap| gap.detail == "thrift-written-owner-or-construct-unavailable"
                        && !spelling(text, &gap.source).is_empty()
                )
        );
    }
}

#[test]
fn annotations_constants_and_permissive_strings_keep_semantic_limitations() {
    let text = "namespace cpp \"sample::api\"\nconst string TEXT = \"bad\\q\"\nstruct Item { 1: string name (note = \"x\"); }";
    let output = analyze_at("schema.thrift", text);
    definition(
        &output,
        text,
        "cpp \"sample::api\"",
        EntityKind::Namespace,
        "namespace cpp \"sample::api\"",
    );
    definition(
        &output,
        text,
        "TEXT",
        EntityKind::Constant,
        "const string TEXT",
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.detail == "thrift-string-escape-and-newline-validation-unavailable"
                    && spelling(text, &gap.source) == "\"bad\\q\""
            )
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail
                == "thrift-written-directive-annotation-or-value-semantics-unavailable"
                && spelling(text, &gap.source).starts_with("(note"))
    );
    let performs_text = "service Store { performs Chat; }";
    let performs_output = analyze_at("schema.thrift", performs_text);
    assert!(
        performs_output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail
                == "thrift-written-directive-annotation-or-value-semantics-unavailable"
                && spelling(performs_text, &gap.source) == performs_text)
    );
}

#[test]
fn comments_strings_case_changes_and_empty_sources_preserve_source_identity() {
    let before = "// 🌐 struct Hidden {}\r\nconst string TEXT = \"struct Fake {}\";\r\nstruct Item { 1: string value; }\r\n";
    let output = analyze_at("schema.thrift", before);
    definition(&output, before, "Item", EntityKind::Struct, "struct Item");
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| matches!(entity.canonical_name.as_str(), "Hidden" | "Fake"))
    );
    let after = before.replace("Item", "item");
    let updated = analyze_at("schema.thrift", &after);
    definition(&updated, &after, "item", EntityKind::Struct, "struct item");
    assert_ne!(
        output.document().files[0].content_hash,
        updated.document().files[0].content_hash
    );
    for text in ["", "// no declarations\n"] {
        let output = analyze_at("schema.thrift", text);
        assert!(
            output
                .document()
                .entities
                .iter()
                .all(|entity| entity.kind == EntityKind::Module)
        );
    }
}
