//! JSON Schema source ownership is gated by an explicit supported dialect.
//! Original unquoted leaves and exact pointer witnesses remain independent of
//! generic JSON properties, source retrieval and global spelling resolution.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, EntityKind, EntityRecord, ExtensionSupport,
    IrLimits, NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, ProducerIdentity,
    RelationEndpoint, RelationPredicate, SourceRef, SourceSpan, derive_relation_record_id,
    validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::{TempDir, tempdir};

const ORIGINAL: &str = include_str!("fixtures/json_schema/test-schema.json");
const ORIGINAL_PATH: &str = "schema.json";

struct Fixture {
    _directory: TempDir,
    root: RepositoryRoot,
}

impl Fixture {
    fn new(sources: &[(&str, &str)]) -> Self {
        let directory = tempdir().unwrap();
        for (path, text) in sources {
            let destination = directory.path().join(path);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::write(destination, text.as_bytes()).unwrap();
        }
        let repository = derive_repository(b"json-schema-explicit-module-ownership-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        Self {
            _directory: directory,
            root,
        }
    }

    fn analyze(&self, path: &str) -> AnalysisOutput {
        self.analyze_options(path, IrLimits::default(), &Cancellation::new())
            .unwrap()
    }

    fn analyze_options(
        &self,
        path: &str,
        ir: IrLimits,
        cancellation: &Cancellation,
    ) -> Result<AnalysisOutput, rootlight_adapter_sdk::AdapterError> {
        self.analyze_encoded_options(path, "utf-8", ir, cancellation)
    }

    fn analyze_encoded_options(
        &self,
        path: &str,
        encoding: &str,
        ir: IrLimits,
        cancellation: &Cancellation,
    ) -> Result<AnalysisOutput, rootlight_adapter_sdk::AdapterError> {
        self.analyze_encoded_with_syntax_ceiling(path, encoding, ir, cancellation, 16_384)
    }

    fn analyze_encoded_with_syntax_ceiling(
        &self,
        path: &str,
        encoding: &str,
        ir: IrLimits,
        cancellation: &Cancellation,
        maximum_syntax_nodes: usize,
    ) -> Result<AnalysisOutput, rootlight_adapter_sdk::AdapterError> {
        let snapshot = self
            .root
            .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 16_384)
            .unwrap();
        let source = SourceRef::new(
            self.root.repository(),
            GenerationId::from_bytes([21; 20]),
            SourceSpan::new(
                snapshot.file(),
                0,
                u64::try_from(snapshot.content().len()).unwrap(),
            )
            .unwrap(),
            snapshot.content_hash(),
            None,
        );
        let provider = Arc::new(
            TreeSitterProvider::new(
                RuntimeConfig::new(
                    16_384,
                    maximum_syntax_nodes,
                    128,
                    32,
                    64,
                    1,
                    16 * 1024 * 1024,
                    ParserSettings::new(512).unwrap(),
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let analyzer = TreeSitterAnalyzer::new(
            provider,
            ProducerIdentity::new(
                "json-schema-module-contract",
                "1.0",
                content_hash(b"json-schema-module-configuration"),
            )
            .unwrap(),
            LanguageId::new("json-schema").unwrap(),
            "json-schema-written-module",
            content_hash(b"json-schema-module-contract-binary"),
        )
        .unwrap();
        let stream = StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap();
        let limits = AnalysisLimits::new(
            16_384,
            maximum_syntax_nodes,
            128,
            32,
            16 * 1024 * 1024,
            stream.clone(),
            stream,
            ir,
        )
        .unwrap();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("json-schema").unwrap(),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"json-schema-module-build-context")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        )?;
        validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
        Ok(output)
    }
}

fn definition(document: &NormalizedIrDocument, start: u64, end: u64) -> &EntityRecord {
    let occurrences: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(occurrences.len(), 1, "exact definition at {start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = &occurrences[0].target else {
        panic!("definition has no exact entity");
    };
    document
        .entities
        .iter()
        .find(|entity| entity.id == *symbol)
        .unwrap()
}

fn edge<'a>(
    document: &'a NormalizedIrDocument,
    predicate: RelationPredicate,
    owner: &EntityRecord,
    member: &EntityRecord,
) -> &'a rootlight_ir::RelationRecord {
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == predicate
                && edge.subject == RelationEndpoint::Entity(owner.id)
                && edge.object == RelationEndpoint::Entity(member.id)
        })
        .collect();
    assert_eq!(
        edges.len(),
        1,
        "{predicate:?}: {} -> {}",
        owner.canonical_name,
        member.canonical_name
    );
    assert_eq!(edges[0].id, derive_relation_record_id(edges[0]).unwrap());
    edges[0]
}

fn at<'a>(
    document: &'a NormalizedIrDocument,
    source: &str,
    token: &str,
    ordinal: usize,
) -> &'a EntityRecord {
    let begin = source.match_indices(token).nth(ordinal).unwrap().0;
    definition(
        document,
        u64::try_from(begin + 1).unwrap(),
        u64::try_from(begin + token.len() - 1).unwrap(),
    )
}

#[test]
fn original_four_definitions_and_three_relations_use_schema_owned_physical_witnesses() {
    assert_eq!(ORIGINAL.len(), 5849);
    let fixture = Fixture::new(&[(ORIGINAL_PATH, ORIGINAL)]);
    let output = fixture.analyze(ORIGINAL_PATH);
    assert_original_schema(output.document());
}

#[test]
fn original_schema_accepts_service_syntax_ceiling_without_false_capture_exhaustion() {
    let fixture = Fixture::new(&[(ORIGINAL_PATH, ORIGINAL)]);
    let output = fixture
        .analyze_encoded_with_syntax_ceiling(
            ORIGINAL_PATH,
            "utf-8",
            IrLimits::default(),
            &Cancellation::new(),
            usize::MAX,
        )
        .expect("the service syntax ceiling must not exhaust proof work before scanning source");
    assert_original_schema(output.document());
}

fn assert_original_schema(document: &NormalizedIrDocument) {
    let model = definition(document, 4852, 4856);
    let data = definition(document, 5413, 5417);
    let valid = definition(document, 5571, 5576);
    let tests = definition(document, 960, 965);
    for (entity, name, kind) in [
        (model, "test", EntityKind::Struct),
        (data, "data", EntityKind::Field),
        (valid, "valid", EntityKind::Field),
        (tests, "tests", EntityKind::Field),
    ] {
        assert_eq!(entity.language, "json-schema");
        assert_eq!(entity.canonical_name, name);
        assert_eq!(entity.kind, kind);
        let source = entity.evidence.source.as_ref().unwrap();
        assert_eq!(source.content_hash(), content_hash(ORIGINAL.as_bytes()));
        assert_eq!(source.generation(), GenerationId::from_bytes([21; 20]));
    }
    edge(document, RelationPredicate::Contains, model, data);
    edge(document, RelationPredicate::Contains, model, valid);
    assert_eq!(data.container, Some(ContainerRef::Entity(model.id)));
    assert_eq!(valid.container, Some(ContainerRef::Entity(model.id)));
    let uses = edge(document, RelationPredicate::UsesType, tests, model);
    let source = uses.evidence.source.as_ref().unwrap();
    assert!(source.span().start_byte() <= 1133 && source.span().end_byte() >= 1137);
    assert_eq!(source.content_hash(), content_hash(ORIGINAL.as_bytes()));
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() <= 1133
                && site.source.span().end_byte() >= 1137)
    );
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::TypeUse
                && site.source.span().start_byte() <= 1133
                && site.source.span().end_byte() >= 1137
                && site.target == OccurrenceTarget::Resolved { symbol: model.id })
    );
}

#[test]
fn schema_map_names_spelling_applicators_keep_declarations_and_local_refs() {
    for name in ["items", "not", "contains"] {
        let source = r##"{
          "$schema":"https://json-schema.org/draft/2020-12/schema",
          "$defs":{"$NAME":{"type":"object","properties":{"leaf":{}}}},
          "properties":{"$NAME":{"$ref":"#/$defs/$NAME"}},
          "examples":[{
            "$defs":{"$NAME":{"type":"object","properties":{"annotation_only":{}}}},
            "properties":{"$NAME":{"$ref":"#/$defs/$NAME"}}
          }],
          "default":{"properties":{"$NAME":{"properties":{"default_only":{}}}}},
          "unknown":{"$defs":{"$NAME":{"properties":{"unknown_only":{}}}}}
        }"##
        .replace("$NAME", name);
        let fixture = Fixture::new(&[("schema.json", &source)]);
        let output = fixture.analyze("schema.json");
        let document = output.document();
        let token = format!("\"{name}\"");
        let model = at(document, &source, &token, 0);
        let field = at(document, &source, &token, 1);
        let leaf = at(document, &source, "\"leaf\"", 0);
        assert_eq!(model.kind, EntityKind::Struct);
        assert_eq!(field.kind, EntityKind::Field);
        assert_eq!(leaf.container, Some(ContainerRef::Entity(model.id)));
        edge(document, RelationPredicate::Contains, model, leaf);
        edge(document, RelationPredicate::UsesType, field, model);
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == name)
                .count(),
            2
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::UsesType)
                .count(),
            1
        );
        for absent in ["annotation_only", "default_only", "unknown_only"] {
            assert!(
                !document
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == absent)
            );
        }
    }
}

#[test]
fn physical_pointer_resolves_repeated_names_and_ref_siblings_without_annotation_promotion() {
    let source = r##"{
      "$schema":"https://json-schema.org/draft/2020-12/schema",
      "$defs":{
        "first":{"type":"object","properties":{"same":{"type":"string"}}},
        "second":{"type":"object","properties":{"same":{"type":"number"}}}
      },
      "properties":{"values":{"$ref":"#/$defs/second","properties":{"sibling":{}}}},
      "examples":[{"$defs":{"fake":{"type":"object"}},"properties":{"ghost":{}}}],
      "default":{"properties":{"pretend":{}}}
    }"##;
    let fixture = Fixture::new(&[("schema.json", source), ("other.json", source)]);
    let output = fixture.analyze("schema.json");
    let document = output.document();
    let first = at(document, source, "\"first\"", 0);
    let second = at(document, source, "\"second\"", 0);
    let same_first = at(document, source, "\"same\"", 0);
    let same_second = at(document, source, "\"same\"", 1);
    let values = at(document, source, "\"values\"", 0);
    let sibling = at(document, source, "\"sibling\"", 0);
    assert_ne!(same_first.id, same_second.id);
    edge(document, RelationPredicate::Contains, first, same_first);
    edge(document, RelationPredicate::Contains, second, same_second);
    edge(document, RelationPredicate::Contains, values, sibling);
    edge(document, RelationPredicate::UsesType, values, second);
    assert!(
        !document
            .relations
            .iter()
            .any(|row| row.predicate == RelationPredicate::UsesType
                && row.object == RelationEndpoint::Entity(first.id))
    );
    for absent in ["fake", "ghost", "pretend"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == absent)
        );
    }
    let other = fixture.analyze("other.json");
    assert!(
        other
            .document()
            .entities
            .iter()
            .all(|entity| !document.entities.iter().any(|old| old.id == entity.id))
    );
}

#[test]
fn decoded_pointer_tokens_keep_escaped_source_names_and_all_array_positions() {
    let source = r##"{
      "$schema":"https://json-schema.org/draft/2020-12/schema",
      "$defs":{"a/b~c":{"type":"object","properties":{"caf\u00e9":{}}}},
      "properties":{"value":{"items":{"$ref":"#/$defs/a~1b~0c"}}},
      "allOf":[true,false,{"$defs":{"later":{"type":"object"}}}],
      "properties2":{"ignored":{"$ref":"#/$defs/a~1b~0c"}}
    }"##;
    let fixture = Fixture::new(&[("schema.json", source)]);
    let output = fixture.analyze("schema.json");
    let document = output.document();
    let model = at(document, source, "\"a/b~c\"", 0);
    let field = at(document, source, "\"caf\\u00e9\"", 0);
    assert_eq!(model.canonical_name, "a/b~c");
    assert_eq!(field.canonical_name, "café");
    edge(document, RelationPredicate::Contains, model, field);
    edge(
        document,
        RelationPredicate::UsesType,
        at(document, source, "\"value\"", 0),
        model,
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "later")
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "ignored")
    );
}

#[test]
fn missing_ambiguous_external_and_changed_resource_refs_do_not_use_name_fallback() {
    for reference in [
        "#/$defs/missing",
        "#/$defs/model~2",
        "#/$defs/model%ZZ",
        "#/allOf/01",
        "other.json#/$defs/model",
        "https://example.invalid/schema#/$defs/model",
        "#anchor",
    ] {
        let source = format!(
            r##"{{"$schema":"https://json-schema.org/draft/2020-12/schema","$defs":{{"model":{{"type":"object"}}}},"properties":{{"value":{{"$ref":"{reference}"}}}}}}"##
        );
        let fixture = Fixture::new(&[("schema.json", &source)]);
        let output = fixture.analyze("schema.json");
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|row| row.predicate == RelationPredicate::UsesType),
            "{reference}"
        );
        assert!(!output.document().skipped_regions.is_empty(), "{reference}");
    }
    for source in [
        r##"{"$schema":"https://json-schema.org/draft/2020-12/schema","$defs":{"model":{"type":"object"},"mo\u0064el":{"type":"object"}},"properties":{"value":{"$ref":"#/$defs/model"}}}"##,
        r##"{"$schema":"https://json-schema.org/draft/2020-12/schema","$defs":{"model":{"$id":"https://example.invalid/other","type":"object"}},"properties":{"value":{"$ref":"#/$defs/model"}}}"##,
        r##"{"$schema":"https://json-schema.org/draft/2020-12/schema","$defs":{"model":{"type":"object"}},"properties":{"value":{"$id":"nested","$ref":"#/$defs/model"}}}"##,
    ] {
        let fixture = Fixture::new(&[("schema.json", source)]);
        let output = fixture.analyze("schema.json");
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|row| row.predicate == RelationPredicate::UsesType)
        );
        assert!(!output.document().skipped_regions.is_empty());
    }
}

#[test]
fn native_recovery_and_unsupported_dialects_do_not_publish_schema_binding_proof() {
    for source in [
        r##"{"$schema":"https://json-schema.org/draft/2020-12/schema","$defs":{"model":{"type":"object"}},"properties":{"value":{"$ref":"#/$defs/model"}},}"##,
        r##"{"$schema":"http://json-schema.org/draft-07/schema#","$defs":{"model":{"type":"object"}},"properties":{"value":{"$ref":"#/$defs/model"}}}"##,
        r##"{"$schema":"https://json-schema.org/draft/2020-12/schema","$defs":{"model":{"type":"object"}},"properties":{"value":{"$ref":"#/$defs/model"}}} {}"##,
    ] {
        let fixture = Fixture::new(&[("schema.json", source)]);
        let output = fixture.analyze("schema.json");
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|row| row.predicate == RelationPredicate::UsesType)
        );
        assert!(
            !output.document().skipped_regions.is_empty(),
            "missing explicit analysis gap for {source}"
        );
    }
}

#[test]
fn source_owned_schema_edges_observe_actual_relation_quota_and_cancellation() {
    let fixture = Fixture::new(&[(ORIGINAL_PATH, ORIGINAL)]);
    let output = fixture.analyze(ORIGINAL_PATH);
    let mut limits = IrLimits::default();
    limits.max_relations = output.document().relations.len().checked_sub(1).unwrap();
    assert!(
        fixture
            .analyze_options(ORIGINAL_PATH, limits, &Cancellation::new())
            .is_err()
    );
    let mut limits = IrLimits::default();
    limits.max_nested_items_per_record = 2;
    assert!(
        fixture
            .analyze_options(ORIGINAL_PATH, limits, &Cancellation::new())
            .is_err()
    );
    let cancellation = Cancellation::new();
    cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(
        fixture
            .analyze_options(ORIGINAL_PATH, IrLimits::default(), &cancellation)
            .is_err()
    );
}

#[test]
fn original_schema_definitions_and_reference_evidence_map_to_exact_utf16_bytes() {
    assert!(ORIGINAL.is_ascii());
    for (encoding, little) in [("utf-16le", true), ("utf-16be", false)] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in ORIGINAL.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let fixture = Fixture::new(&[(ORIGINAL_PATH, "")]);
        fs::write(fixture._directory.path().join(ORIGINAL_PATH), &bytes).unwrap();
        let output = fixture
            .analyze_encoded_options(
                ORIGINAL_PATH,
                encoding,
                IrLimits::default(),
                &Cancellation::new(),
            )
            .unwrap();
        let document = output.document();
        let model = definition(document, 2 + 4852 * 2, 2 + 4856 * 2);
        let data = definition(document, 2 + 5413 * 2, 2 + 5417 * 2);
        let valid = definition(document, 2 + 5571 * 2, 2 + 5576 * 2);
        let tests = definition(document, 2 + 960 * 2, 2 + 965 * 2);
        edge(document, RelationPredicate::Contains, model, data);
        edge(document, RelationPredicate::Contains, model, valid);
        let uses = edge(document, RelationPredicate::UsesType, tests, model);
        let source = uses.evidence.source.as_ref().unwrap();
        assert_eq!(source.content_hash(), content_hash(&bytes));
        assert!(
            source.span().start_byte() <= 2 + 1133 * 2 && source.span().end_byte() >= 2 + 1137 * 2
        );
        assert_eq!(
            &bytes[usize::try_from(2 + 4852 * 2).unwrap()..usize::try_from(2 + 4856 * 2).unwrap()],
            &if little {
                vec![b't', 0, b'e', 0, b's', 0, b't', 0]
            } else {
                vec![0, b't', 0, b'e', 0, b's', 0, b't']
            }
        );
    }
}

#[test]
fn byte_order_mark_and_crlf_preserve_source_addresses_and_model_identity() {
    let fixture = Fixture::new(&[(ORIGINAL_PATH, ORIGINAL)]);
    let initial = fixture.analyze(ORIGINAL_PATH);
    let original_model = definition(initial.document(), 4852, 4856).id;
    let crlf = ORIGINAL.replace('\n', "\r\n");
    for (text, bom) in [(ORIGINAL, false), (ORIGINAL, true), (crlf.as_str(), false)] {
        let mut bytes = if bom {
            vec![0xef, 0xbb, 0xbf]
        } else {
            Vec::new()
        };
        bytes.extend_from_slice(text.as_bytes());
        let fixture = Fixture::new(&[(ORIGINAL_PATH, "")]);
        fs::write(fixture._directory.path().join(ORIGINAL_PATH), &bytes).unwrap();
        let output = fixture.analyze(ORIGINAL_PATH);
        let document = output.document();
        let offset = if bom { 3 } else { 0 };
        let named = |token: &str| {
            let start = text.find(&format!("\"{token}\": {{")).unwrap() + 1 + offset;
            definition(
                document,
                u64::try_from(start).unwrap(),
                u64::try_from(start + token.len()).unwrap(),
            )
        };
        let model = named("test");
        assert_eq!(model.id, original_model);
        edge(document, RelationPredicate::Contains, model, named("data"));
        edge(document, RelationPredicate::Contains, model, named("valid"));
        let uses = edge(document, RelationPredicate::UsesType, named("tests"), model);
        assert_eq!(
            uses.evidence.source.as_ref().unwrap().content_hash(),
            content_hash(&bytes)
        );
    }
}
