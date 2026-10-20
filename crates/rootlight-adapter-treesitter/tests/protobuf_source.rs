//! Protobuf contracts retain written owners, exact source spans and local type uses.

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
    AnalysisTier, BuildContextIdentity, EntityKind, ExtensionSupport, IrLimits, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate, SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = "// 🌐 physical comment\r\nsyntax = \"proto3\";\r\npackage sample.api;\r\nimport public \"external.proto\";\r\nmessage Request {\r\n  message Nested { string value = 1; }\r\n  enum State { READY = 0; }\r\n  Nested item = 1;\r\n  map<string, .sample.api.Request.Nested> entries = 2;\r\n  oneof choice { Nested selected = 3; string text = 4; }\r\n}\r\nservice Gateway { rpc Fetch(stream Request) returns (Request.Nested); }\r\n// message Phantom {}\r\n";

fn analyze(text: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    analyze_at("schema.proto", text)
}

fn analyze_at(path: &str, text: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    let directory = tempdir().unwrap();
    let file = directory.path().join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
    let repository = derive_repository(b"protobuf-written-source-contract").id();
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
            "protobuf-source-contract",
            "1.0",
            content_hash(b"protobuf-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("protobuf").unwrap(),
        "protobuf-static-contract",
        content_hash(b"protobuf-contract-binary"),
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
        LanguageId::new("protobuf").unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"protobuf-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )
    .unwrap()
}

#[test]
fn documentary_tokens_keep_exact_evidence_without_identifier_failures() {
    let output = analyze(SOURCE);
    let document = output.document();
    assert!(
        document
            .occurrences
            .iter()
            .any(|occurrence| { occurrence.role == OccurrenceRole::Documentation })
    );
    assert!(
        document
            .occurrences
            .iter()
            .any(|occurrence| { occurrence.role == OccurrenceRole::StringEvidence })
    );
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|region| { region.detail == "protobuf-reference-name-unavailable" }),
        "comments and quoted strings do not require identifier decoding"
    );
    for occurrence in document.occurrences.iter().filter(|occurrence| {
        matches!(
            occurrence.role,
            OccurrenceRole::Documentation | OccurrenceRole::StringEvidence
        )
    }) {
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(SOURCE.as_bytes())
        );
        let span = occurrence.source.span();
        let raw = SOURCE
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        let expected_label = if occurrence.role == OccurrenceRole::Documentation {
            assert!(raw.starts_with("//"));
            "protobuf.comment.comment"
        } else {
            assert!(raw.starts_with('"') && raw.ends_with('"'));
            "protobuf.string.string"
        };
        assert_eq!(occurrence.syntax_kind, expected_label);
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|region| { region.detail == "protobuf-import-target-unavailable" })
    );
}

#[test]
fn written_definitions_containment_and_type_uses_keep_exact_source_hashes() {
    let output = analyze(SOURCE);
    let document = output.document();
    for (name, kind) in [
        ("sample.api", EntityKind::Namespace),
        ("Request", EntityKind::Struct),
        ("Nested", EntityKind::Struct),
        ("State", EntityKind::Enum),
        ("READY", EntityKind::Constant),
        ("item", EntityKind::Field),
        ("entries", EntityKind::Field),
        ("choice", EntityKind::Union),
        ("selected", EntityKind::Field),
        ("Gateway", EntityKind::Interface),
        ("Fetch", EntityKind::Method),
    ] {
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name && entity.kind == kind),
            "{name}"
        );
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Phantom")
    );
    for occurrence in &document.occurrences {
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(SOURCE.as_bytes())
        );
        let span = occurrence.source.span();
        let raw = SOURCE
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        if occurrence.syntax_kind == "protobuf.name.definition" {
            assert!(
                document
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == raw)
            );
        }
        if occurrence.syntax_kind == "protobuf.type.reference"
            && raw == ".sample.api.Request.Nested"
        {
            let target = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Nested")
                .unwrap();
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol: target.id }
            );
            assert!(document.relations.iter().any(|relation| relation.predicate
                == RelationPredicate::UsesType
                && relation.subject == RelationEndpoint::Occurrence(occurrence.id)
                && relation.object == RelationEndpoint::Entity(target.id)
                && relation.evidence.source.as_ref() == Some(&occurrence.source)));
        }
    }
    let request = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Request")
        .unwrap();
    let nested = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Nested")
        .unwrap();
    assert!(
        document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(request.id)
                && relation.object == RelationEndpoint::Entity(nested.id))
    );
    assert!(
        document
            .occurrences
            .iter()
            .any(
                |occurrence| occurrence.syntax_kind == "protobuf.written_import.import"
                    && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            )
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "protobuf-import-target-unavailable")
    );
    assert!(document.skipped_regions.iter().any(|gap| gap.detail == "protobuf-cross-file-import-descriptor-option-extension-and-wire-validation-unavailable"));
}

#[test]
fn same_spelling_in_different_scopes_does_not_create_a_guessed_type_edge() {
    let text = "syntax = \"proto3\"; package sample; message Outer { message Child {} message Inner { message Child {} Child value = 1; } } message Other { External value = 1; }";
    let output = analyze(text);
    let document = output.document();
    let child_uses = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "protobuf.type.reference")
        .collect::<Vec<_>>();
    assert_eq!(child_uses.len(), 2);
    let inner_child = document
        .entities
        .iter()
        .find(|entity| entity.qualified_name.ends_with("Outer::Inner::Child"))
        .unwrap();
    assert!(child_uses.iter().any(|occurrence| occurrence.target
        == OccurrenceTarget::Resolved {
            symbol: inner_child.id
        }));
    assert!(
        child_uses
            .iter()
            .any(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "protobuf-type-target-unavailable")
    );
}

#[test]
fn physical_module_and_nested_type_parents_do_not_enter_protobuf_namespaces() {
    for path in ["schema.proto", "schemas/types.proto"] {
        for package in ["", "sample.api"] {
            let package_clause = if package.is_empty() {
                String::new()
            } else {
                format!("package {package}; ")
            };
            let prefix = if package.is_empty() {
                String::new()
            } else {
                format!("{package}.")
            };
            let nested_name = format!(".{prefix}Outer.Nested");
            let enum_name = format!(".{prefix}Outer.State");
            let text = format!(
                "syntax = \"proto3\"; {package_clause}message Outer {{ message Nested {{}} enum State {{ ZERO = 0; }} Nested item = 1; }} message Holder {{ {nested_name} value = 1; {enum_name} state = 2; }}"
            );
            let output = analyze_at(path, &text);
            let document = output.document();
            let entity = |name, kind| {
                document
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == name && entity.kind == kind)
                    .unwrap()
            };
            let module = entity(path, EntityKind::Module);
            let outer = entity("Outer", EntityKind::Struct);
            let nested = entity("Nested", EntityKind::Struct);
            let state = entity("State", EntityKind::Enum);
            assert_eq!(
                outer.container,
                Some(rootlight_ir::ContainerRef::Entity(module.id))
            );
            assert_eq!(
                nested.container,
                Some(rootlight_ir::ContainerRef::Entity(outer.id))
            );
            assert_eq!(
                state.container,
                Some(rootlight_ir::ContainerRef::Entity(outer.id))
            );
            assert_eq!(nested.qualified_name, format!("{path}::Outer::Nested"));
            for (written, target) in [
                ("Nested", nested),
                (nested_name.as_str(), nested),
                (enum_name.as_str(), state),
            ] {
                let uses = document
                    .occurrences
                    .iter()
                    .filter(|occurrence| {
                        let span = occurrence.source.span();
                        occurrence.syntax_kind == "protobuf.type.reference"
                            && text.get(
                                usize::try_from(span.start_byte()).unwrap()
                                    ..usize::try_from(span.end_byte()).unwrap(),
                            ) == Some(written)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(uses.len(), 1, "{path} / {package} / {written}");
                let occurrence = uses[0];
                assert_eq!(
                    occurrence.source.content_hash(),
                    content_hash(text.as_bytes())
                );
                assert_eq!(
                    occurrence.target,
                    OccurrenceTarget::Resolved { symbol: target.id }
                );
                assert!(document.relations.iter().any(|relation| relation.predicate
                    == RelationPredicate::UsesType
                    && relation.subject == RelationEndpoint::Occurrence(occurrence.id)
                    && relation.object == RelationEndpoint::Entity(target.id)
                    && relation.evidence.source.as_ref() == Some(&occurrence.source)));
            }
        }
    }
}

#[test]
fn absolute_extendees_parse_without_claiming_extension_bindings() {
    for (extendee, nested) in [
        (".google.protobuf.EnumValueOptions", false),
        (".google.protobuf.FieldOptions", false),
        (".google.protobuf.MessageOptions", false),
        (".google.protobuf.ServiceOptions", false),
        (".google.protobuf.MethodOptions", false),
        (".google.protobuf.EnumOptions", false),
        ("google.protobuf.FieldOptions", false),
        ("External", false),
        (".sample.External", true),
    ] {
        let extension = format!("extend {extendee} {{ optional string extension_value = 50000; }}");
        let declaration = if nested {
            format!("message Container {{ {extension} }}")
        } else {
            extension.clone()
        };
        let text = format!("syntax = \"proto2\"; {declaration} message Following {{}}");
        let output = analyze(&text);
        let document = output.document();
        assert_eq!(document.files.len(), 1);
        assert_eq!(
            document.files[0].content_hash,
            content_hash(text.as_bytes())
        );
        assert_eq!(
            document.files[0].byte_length,
            u64::try_from(text.len()).unwrap()
        );
        assert!(
            document
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != "syntax-error-recovery"),
            "{extendee} / nested={nested}"
        );
        assert!(document.entities.iter().any(|entity| {
            entity.canonical_name == "Following" && entity.kind == EntityKind::Struct
        }));
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "extension_value")
        );
        let gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "protobuf-extension-owner-and-binding-unavailable")
            .collect::<Vec<_>>();
        assert_eq!(gaps.len(), 1);
        let source = &gaps[0].source;
        assert_eq!(source.content_hash(), content_hash(text.as_bytes()));
        assert_eq!(
            text.get(
                usize::try_from(source.span().start_byte()).unwrap()
                    ..usize::try_from(source.span().end_byte()).unwrap()
            ),
            Some(extension.as_str())
        );
        for occurrence in &document.occurrences {
            assert_eq!(
                occurrence.source.content_hash(),
                content_hash(text.as_bytes())
            );
            let span = occurrence.source.span();
            let raw = text
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                )
                .unwrap();
            assert_eq!(occurrence.syntactic_text_hash, content_hash(raw.as_bytes()));
        }
    }
}

#[test]
fn optional_absolute_extendee_dot_does_not_admit_malformed_names() {
    for text in [
        "package .sample;",
        "extend . { optional string extension_value = 50000; }",
        "extend ..sample.External { optional string extension_value = 50000; }",
    ] {
        let output = analyze(text);
        let document = output.document();
        assert_eq!(
            document.files[0].content_hash,
            content_hash(text.as_bytes())
        );
        assert_eq!(
            document.files[0].byte_length,
            u64::try_from(text.len()).unwrap()
        );
        assert!(
            document
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "syntax-error-recovery"),
            "{text}"
        );
    }
}
