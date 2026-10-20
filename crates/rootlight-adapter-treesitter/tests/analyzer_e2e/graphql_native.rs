//! GraphQL public-boundary regressions pin authored names, owner spans and gaps.
//! The cases assert source evidence rather than schema validation or execution.

use super::*;

const GRAPHQL: LanguageCase = LanguageCase {
    name: "graphql",
    path: "schema.graphql",
    frontend: "tree-sitter-graphql-0.0.1",
    source: "type Query { value: String }\n",
    generated: false,
    body_before: "value: String",
    body_after: "other: String",
};

const SDL: &str = "scalar ID\nscalar String\ninterface Named { name: String! }\ntype User implements Named { name: String! friend(limit: ID): User }\ninput Filter { owner: ID }\nenum Tone { LIGHT DARK }\nunion Search = User\ndirective @tag(reason: String) on FIELD_DEFINITION\nschema { query: User }\n";

#[test]
fn graphql_storefront_source_preserves_all_finite_definition_and_relation_witnesses() {
    let source = include_str!("../fixtures/graphql/storefront.graphql");
    assert_eq!(source.len(), 1531);
    let result = output(source);
    // Common field spellings recur in independent types. The authored physical
    // definition witness selects the entity, never a product result ordering.
    let definition_at = |name: &str, kind, start, end| {
        let matches = result
            .document()
            .occurrences
            .iter()
            .filter(|row| {
                row.role == OccurrenceRole::Definition
                    && row.source.span().start_byte() == start
                    && row.source.span().end_byte() == end
                    && text(source, &row.source) == name
            })
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{name} at {start}..{end}");
        let OccurrenceTarget::Resolved { symbol } = matches[0].target else {
            panic!("authored definition must have its own entity");
        };
        let entity = result
            .document()
            .entities
            .iter()
            .find(|row| row.id == symbol)
            .unwrap();
        assert_eq!(entity.canonical_name, name);
        assert_eq!(entity.kind, kind);
        entity
    };
    let query = definition_at("Query", EntityKind::Struct, 406, 411);
    let field = definition_at("viewer", EntityKind::Field, 584, 590);
    let viewer = definition_at("Viewer", EntityKind::Struct, 814, 820);
    let id = definition_at("id", EntityKind::Field, 826, 828);
    for (parent, child) in [(query, field), (viewer, id)] {
        assert!(result.document().relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(parent.id)
                && relation.object == RelationEndpoint::Entity(child.id)
        }));
    }
    assert!(result.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::UsesType
            && relation.subject == RelationEndpoint::Entity(field.id)
            && relation.object == RelationEndpoint::Entity(viewer.id)
            && relation.evidence.source.as_ref().is_some_and(|source_ref| {
                source_ref.span().start_byte() <= 592 && source_ref.span().end_byte() >= 598
            })
    }));
    assert!(!result.document().occurrences.iter().any(|row| {
        row.role == OccurrenceRole::Definition
            && row.source.span().start_byte() < 802
            && row.source.span().end_byte() > 792
    }));
}

fn text<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
}

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(GRAPHQL, source.as_bytes());
    let limits = limits();
    let result = analyze(
        &analyzer(&provider, GRAPHQL),
        &request(&fixture.snapshot, &fixture.source, GRAPHQL, &limits),
        &ExtensionSupport::default(),
    );
    let doc = result.document();
    validate_ir_document(doc, limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(doc.files.len(), 1);
    assert_eq!(doc.files[0].language, "graphql");
    assert_eq!(doc.files[0].content_hash, content_hash(source.as_bytes()));
    assert_eq!(
        doc.files[0].byte_length,
        u64::try_from(source.len()).unwrap()
    );
    assert_eq!(doc.provenance.len(), 1);
    for occurrence in &doc.occurrences {
        assert_eq!(occurrence.source.repository(), doc.repository);
        assert_eq!(occurrence.source.generation(), doc.generation);
        assert_eq!(occurrence.source.span().file(), doc.files[0].id);
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(text(source, &occurrence.source).as_bytes())
        );
        assert_eq!(occurrence.provenance, doc.provenance[0].id);
    }
    for gap in &doc.skipped_regions {
        assert_eq!(gap.source.content_hash(), content_hash(source.as_bytes()));
        let _ = text(source, &gap.source);
    }
    result
}

fn entity<'a>(
    result: &'a AnalysisOutput,
    name: &str,
    kind: EntityKind,
) -> &'a rootlight_ir::EntityRecord {
    let mut found = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name && entity.kind == kind);
    let entity = found
        .next()
        .unwrap_or_else(|| panic!("missing {kind:?} {name}"));
    assert!(found.next().is_none(), "ambiguous test entity {name}");
    entity
}

fn exact_definition(
    result: &AnalysisOutput,
    source: &str,
    name: &str,
    kind: EntityKind,
    marker: &str,
) {
    let owner = entity(result, name, kind);
    let definitions = result.document().occurrences.iter().filter(|occurrence|
        occurrence.role == OccurrenceRole::Definition
        && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == owner.id))
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1, "{name}");
    let occurrence = definitions[0];
    let start = source.find(marker).unwrap() + marker.find(name).unwrap();
    assert_eq!(
        occurrence.source.span().start_byte(),
        u64::try_from(start).unwrap()
    );
    assert_eq!(
        occurrence.source.span().end_byte(),
        u64::try_from(start + name.len()).unwrap()
    );
    assert_eq!(text(source, &occurrence.source), name);
    let context = occurrence.evidence.source.as_ref().unwrap().span();
    assert!(context.start_byte() <= occurrence.source.span().start_byte());
    assert!(context.end_byte() >= occurrence.source.span().end_byte());
}

#[test]
fn graphql_sdl_names_have_exact_written_definition_spans() {
    let result = output(SDL);
    for (name, kind, marker) in [
        ("ID", EntityKind::TypeAlias, "scalar ID"),
        ("Named", EntityKind::Interface, "interface Named"),
        ("User", EntityKind::Struct, "type User"),
        ("Filter", EntityKind::Struct, "input Filter"),
        ("Tone", EntityKind::Enum, "enum Tone"),
        ("Search", EntityKind::Union, "union Search"),
        ("tag", EntityKind::Function, "directive @tag"),
        ("friend", EntityKind::Field, "friend(limit"),
        ("limit", EntityKind::Parameter, "limit: ID"),
        ("owner", EntityKind::Field, "owner: ID"),
        ("LIGHT", EntityKind::Constant, "LIGHT DARK"),
    ] {
        exact_definition(&result, SDL, name, kind, marker);
    }
}

#[test]
fn graphql_contains_uses_lexical_field_argument_and_enum_owners() {
    let result = output(SDL);
    for (parent_name, parent_kind, child_name, child_kind) in [
        ("User", EntityKind::Struct, "friend", EntityKind::Field),
        ("friend", EntityKind::Field, "limit", EntityKind::Parameter),
        ("Filter", EntityKind::Struct, "owner", EntityKind::Field),
        ("Tone", EntityKind::Enum, "LIGHT", EntityKind::Constant),
        ("tag", EntityKind::Function, "reason", EntityKind::Parameter),
    ] {
        let parent = entity(&result, parent_name, parent_kind);
        let child = entity(&result, child_name, child_kind);
        assert_eq!(
            child.container,
            Some(rootlight_ir::ContainerRef::Entity(parent.id))
        );
        assert!(
            result
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(parent.id)
                    && relation.object == RelationEndpoint::Entity(child.id))
        );
    }
}

#[test]
fn graphql_named_type_uses_link_only_unique_document_declarations() {
    let source = "scalar ID\ntype User { id: ID }\nunion Result = User\ninput Filter { item: [User!]! missing: Missing }\n";
    let result = output(source);
    let type_uses = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::TypeUse)
        .collect::<Vec<_>>();
    assert_eq!(type_uses.len(), 4);
    for (name, count) in [("ID", 1), ("User", 2), ("Missing", 1)] {
        assert_eq!(
            type_uses
                .iter()
                .filter(|occurrence| text(source, &occurrence.source) == name)
                .count(),
            count
        );
    }
    for occurrence in result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::TypeUse)
    {
        let spelling = text(source, &occurrence.source);
        if matches!(spelling, "User" | "ID") {
            let target = entity(
                &result,
                spelling,
                if spelling == "ID" {
                    EntityKind::TypeAlias
                } else {
                    EntityKind::Struct
                },
            );
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol: target.id }
            );
            assert!(
                result
                    .document()
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::UsesType
                        && relation.subject == RelationEndpoint::Occurrence(occurrence.id)
                        && relation.object == RelationEndpoint::Entity(target.id)
                        && relation.evidence.source.as_ref() == Some(&occurrence.source))
            );
        } else {
            assert_eq!(spelling, "Missing");
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
    }
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::TypeUse
                && text(source, &occurrence.source) == "Missing")
    );
}

#[test]
fn graphql_operation_and_fragment_names_do_not_shadow_the_type_namespace() {
    let source = "type User { value: User }\nquery User($id: User) { User }\nfragment View on User { User }\n";
    let result = output(source);
    let user = entity(&result, "User", EntityKind::Struct);
    exact_definition(&result, source, "User", EntityKind::Function, "query User");
    let type_uses = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::TypeUse)
        .collect::<Vec<_>>();
    assert_eq!(type_uses.len(), 3);
    assert!(
        type_uses
            .iter()
            .all(|occurrence| text(source, &occurrence.source) == "User"
                && occurrence.target == OccurrenceTarget::Resolved { symbol: user.id })
    );
    for occurrence in result.document().occurrences.iter().filter(|occurrence| {
        occurrence.role == OccurrenceRole::Reference && text(source, &occurrence.source) == "User"
    }) {
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
}

#[test]
fn graphql_selections_are_written_uses_not_sdl_field_definitions_or_calls() {
    let source = "scalar ID\ntype User { name: ID }\nquery Read($id: ID!) { alias: user(id: $id) { name } }\nfragment Basic on User { name }\n";
    let result = output(source);
    exact_definition(&result, source, "Read", EntityKind::Function, "query Read");
    exact_definition(
        &result,
        source,
        "Basic",
        EntityKind::Function,
        "fragment Basic",
    );
    exact_definition(&result, source, "id", EntityKind::Variable, "$id: ID");
    let query_start = source.find("query Read").unwrap();
    for name in ["alias", "user", "name"] {
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Reference
                    && occurrence.source.span().start_byte()
                        >= u64::try_from(query_start).unwrap()
                    && text(source, &occurrence.source) == name)
        );
    }
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| matches!(entity.canonical_name.as_str(), "alias" | "user"))
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn graphql_schema_and_anonymous_operations_do_not_invent_names() {
    let source = "type Query { value: String }\nschema { query: Query }\n{ value }\n";
    let result = output(source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .all(|entity| !matches!(entity.canonical_name.as_str(), "schema" | "query"))
    );
    let query = entity(&result, "Query", EntityKind::Struct);
    let root = result
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::TypeUse
                && text(source, &occurrence.source) == "Query"
        })
        .unwrap();
    assert_eq!(root.target, OccurrenceTarget::Resolved { symbol: query.id });
    let start = source.find("query: Query").unwrap() + "query: ".len();
    assert_eq!(
        root.source.span().start_byte(),
        u64::try_from(start).unwrap()
    );
    assert_eq!(
        root.source.span().end_byte(),
        u64::try_from(start + 5).unwrap()
    );
}

#[test]
fn graphql_extensions_keep_exact_negative_regions_and_no_leaked_members() {
    let source = "type Item { base: String }\nextend type Item { extra: String }\n";
    let result = output(source);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "extra")
    );
    let start = source.find("extend type").unwrap();
    let end = source.trim_end().len();
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.source.span().start_byte() == u64::try_from(start).unwrap()
                    && gap.source.span().end_byte() == u64::try_from(end).unwrap()
                    && text(source, &gap.source) == "extend type Item { extra: String }"
            )
    );
}

#[test]
fn graphql_permissive_escapes_and_block_descriptions_keep_ranged_gaps() {
    for description in [
        r#""bad\q""#,
        r#""bad\123""#,
        r#""bad\x41""#,
        "\"\"\"block description\"\"\"",
    ] {
        let source = format!("{description}\ntype Kept {{ field: String }}\n");
        let result = output(&source);
        exact_definition(&result, &source, "Kept", EntityKind::Struct, "type Kept");
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() == 0
                    && gap.source.span().end_byte() == u64::try_from(description.len()).unwrap()
                    && text(&source, &gap.source) == description)
        );
    }
}

#[test]
fn graphql_comments_and_string_decoys_cannot_create_declarations() {
    let source = "# type Fake { invented: ID }\n\"type Pretend { hidden: ID }\"\ntype Actual { value: String }\n";
    let result = output(source);
    assert!(!result.document().entities.iter().any(|entity| matches!(
        entity.canonical_name.as_str(),
        "Fake" | "invented" | "Pretend" | "hidden"
    )));
    exact_definition(&result, source, "Actual", EntityKind::Struct, "type Actual");
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::StringEvidence)
    );
}

#[test]
fn graphql_utf8_crlf_and_case_sensitive_names_keep_byte_coordinates() {
    let source =
        "# π🙂\r\nscalar Name\r\ntype User { _value: Name }\r\ntype user { value: Name }\r\n";
    let result = output(source);
    exact_definition(&result, source, "User", EntityKind::Struct, "type User");
    exact_definition(&result, source, "user", EntityKind::Struct, "type user");
    exact_definition(&result, source, "_value", EntityKind::Field, "_value: Name");
    assert_ne!(
        entity(&result, "User", EntityKind::Struct).id,
        entity(&result, "user", EntityKind::Struct).id
    );
}

#[test]
fn graphql_duplicate_type_names_never_select_an_arbitrary_target() {
    let source =
        "type Item { left: String }\ntype Item { right: String }\ntype Holder { value: Item }\n";
    let result = output(source);
    let uses = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::TypeUse && text(source, &occurrence.source) == "Item"
        })
        .collect::<Vec<_>>();
    assert_eq!(uses.len(), 1);
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn graphql_missing_declaration_names_retain_negative_source_evidence() {
    let source = "type { field: String }\n";
    let result = output(source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .all(|entity| entity.kind == EntityKind::Module)
    );
    assert!(!result.document().skipped_regions.is_empty());
    assert!(result.document().skipped_regions.iter().any(|gap| {
        let spelling = text(source, &gap.source);
        !spelling.is_empty() && source.contains(spelling)
    }));
}

#[test]
fn graphql_source_change_never_reuses_old_name_or_source_hash() {
    let before = output("type Query { before: String }\n");
    let after_source = "type Query { after: String }\n";
    let after = output(after_source);
    assert_ne!(
        before.document().files[0].content_hash,
        after.document().files[0].content_hash
    );
    assert!(
        !after
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "before")
    );
    exact_definition(
        &after,
        after_source,
        "after",
        EntityKind::Field,
        "after: String",
    );
}

#[test]
fn graphql_empty_and_comment_only_sources_keep_exact_file_identity() {
    for source in ["", "# no executable schema text\n"] {
        let result = output(source);
        assert_eq!(
            result.document().files[0].content_hash,
            content_hash(source.as_bytes())
        );
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.kind == EntityKind::Module)
        );
    }
}

use rootlight_adapter_sdk::{ParseOutput, execute_parse};

fn admission_frame(source: &str, spans: &[(u64, u64)]) -> (ParseOutput, AnalysisOutput) {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(GRAPHQL, source.as_bytes());
    let limits = limits();
    let ranges = spans
        .iter()
        .map(|&(start, end)| {
            IncludedRange::new(
                SourceSpan::new(fixture.snapshot.file(), start, end).unwrap(),
                LanguageId::new("graphql").unwrap(),
            )
        })
        .collect();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        LanguageId::new("graphql").unwrap(),
        EncodingId::utf8(),
        ranges,
        AnalysisTier::TierD,
        build_context(),
        &limits,
    )
    .unwrap()
    .with_generated_status(GRAPHQL.generated);
    let parsed = execute_parse(
        provider.as_ref(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let result = analyze(
        &analyzer(&provider, GRAPHQL),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(
        result.document().files[0].content_hash,
        content_hash(source.as_bytes())
    );
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(source.len()).unwrap()
    );
    (parsed, result)
}

fn unresolved_admission_item(result: &AnalysisOutput, source: &str) {
    let uses = result
        .document()
        .occurrences
        .iter()
        .filter(|row| row.role == OccurrenceRole::TypeUse && text(source, &row.source) == "Item")
        .collect::<Vec<_>>();
    assert_eq!(uses.len(), 1);
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    let start = source.find("value: Item").unwrap() + "value: ".len();
    assert_eq!(
        uses[0].source.span().start_byte(),
        u64::try_from(start).unwrap()
    );
    assert_eq!(
        uses[0].source.span().end_byte(),
        u64::try_from(start + 4).unwrap()
    );
    assert_eq!(
        uses[0].source.content_hash(),
        content_hash(source.as_bytes())
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|row| row.predicate == RelationPredicate::UsesType)
    );
    exact_definition(result, source, "Host", EntityKind::Struct, "type Host");
    exact_definition(result, source, "value", EntityKind::Field, "value: Item");
}

#[test]
fn graphql_included_prefix_hiding_a_duplicate_keeps_raw_facts_without_unique_targets() {
    let prefix = "type Item { left: String }\ntype Host { value: Item }\n";
    let source = format!("{prefix}type Item {{ right: String }}\n");
    let end = u64::try_from(prefix.len()).unwrap();
    let (parsed, result) = admission_frame(&source, &[(0, end)]);
    assert_eq!(parsed.report().coverage().status(), CoverageStatus::Bounded);
    assert_eq!(
        parsed.report().coverage().covered_source_bytes(),
        prefix.len()
    );
    assert_eq!(
        parsed.report().coverage().total_source_bytes(),
        source.len()
    );
    unresolved_admission_item(&result, &source);
    exact_definition(
        &result,
        &source,
        "Item",
        EntityKind::Struct,
        "type Item { left",
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "outside-included-ranges"
                && gap.source.span().start_byte() == end
                && gap.source.span().end_byte() == u64::try_from(source.len()).unwrap())
    );
}

#[test]
fn graphql_malformed_duplicate_cannot_leave_a_false_unique_type_target() {
    let source = "type Item { left: String }\ntype Item { broken: }\ntype Host { value: Item }\n";
    let (parsed, result) = admission_frame(source, &[]);
    assert_ne!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(!parsed.diagnostics().is_empty());
    assert!(
        parsed
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "graphql.declaration_unavailable.signature")
    );
    unresolved_admission_item(&result, source);
    exact_definition(
        &result,
        source,
        "Item",
        EntityKind::Struct,
        "type Item { left",
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "graphql-declaration-name-or-owner-unavailable")
    );
}

#[test]
fn graphql_full_span_included_request_does_not_bypass_standalone_binding_admission() {
    let source = "type Item { left: String }\ntype Host { value: Item }\n";
    let (parsed, result) = admission_frame(source, &[(0, u64::try_from(source.len()).unwrap())]);
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        parsed.report().coverage().covered_source_bytes(),
        source.len()
    );
    unresolved_admission_item(&result, source);
    exact_definition(&result, source, "Item", EntityKind::Struct, "type Item");
}

#[test]
fn graphql_utf16_frames_keep_physical_definitions_without_utf8_only_type_targets() {
    let source = "# marker λ😀\r\ntype Item { left: String }\r\ntype Host { value: Item }\r\n";
    for little_endian in [true, false] {
        let mut raw = if little_endian {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            let bytes = if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            };
            raw.extend_from_slice(&bytes);
        }
        let provider = Arc::new(provider());
        let fixture = Fixture::new(GRAPHQL, &raw);
        let limits = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            LanguageId::new("graphql").unwrap(),
            EncodingId::new(if little_endian {
                "utf-16le"
            } else {
                "utf-16be"
            })
            .unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &limits,
        )
        .unwrap()
        .with_generated_status(GRAPHQL.generated);
        let parsed = execute_parse(
            provider.as_ref(),
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        let result = analyze(
            &analyzer(&provider, GRAPHQL),
            &request,
            &ExtensionSupport::default(),
        );
        validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
        assert_eq!(result.document().files[0].content_hash, content_hash(&raw));
        assert_eq!(
            result.document().files[0].byte_length,
            u64::try_from(raw.len()).unwrap()
        );
        let item = entity(&result, "Item", EntityKind::Struct);
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|row| {
                row.role == OccurrenceRole::Definition
                    && row.target == OccurrenceTarget::Resolved { symbol: item.id }
            })
            .unwrap();
        let projected_definition = source.find("type Item").unwrap() + "type ".len();
        let physical_definition =
            2 + 2 * u64::try_from(source[..projected_definition].encode_utf16().count()).unwrap();
        assert_eq!(definition.source.span().start_byte(), physical_definition);
        assert_eq!(definition.source.span().end_byte(), physical_definition + 8);
        assert_eq!(definition.source.content_hash(), content_hash(&raw));
        let projected_use = source.find("value: Item").unwrap() + "value: ".len();
        let physical_use =
            2 + 2 * u64::try_from(source[..projected_use].encode_utf16().count()).unwrap();
        let uses = result
            .document()
            .occurrences
            .iter()
            .filter(|row| {
                row.role == OccurrenceRole::TypeUse
                    && row.source.span().start_byte() == physical_use
            })
            .collect::<Vec<_>>();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].source.span().end_byte(), physical_use + 8);
        assert_eq!(uses[0].source.content_hash(), content_hash(&raw));
        assert!(matches!(
            uses[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|row| row.predicate == RelationPredicate::UsesType)
        );
    }
}
