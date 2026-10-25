//! Regression contracts for project consumption of source-proved inherent owners.
//!
//! Real native facts feed the ordinary project builder; legacy records are captured
//! before reconciliation so owner projection cannot hide a changed symbol recipe.

use super::*;
use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisUnitId, BatchThresholds, BuildTargetId, EncodingId,
    GenerationBoundSnapshot, ProjectAnalysisLimits, StreamLimits,
};
use rootlight_adapter_treesitter::{ParserSettings, RuntimeConfig, TreeSitterProvider};
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::{IrLimits, decode_symbol_identity_claim_envelope};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use std::path::Path;

fn with_parsed(
    text: &str,
    edit: impl FnOnce(&mut Vec<SyntaxFact>),
    inspect: impl FnOnce(ProjectFactsBuilder<'_, '_, '_>),
) {
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    std::fs::create_dir(temporary.path().join("src")).unwrap();
    std::fs::write(temporary.path().join("src/lib.rs"), text).unwrap();
    let repository: RepositoryId = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
    let generation: GenerationId = "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
        .parse()
        .unwrap();
    let root = RepositoryRoot::open(repository, temporary.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("src/lib.rs")).unwrap(),
            64 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let batch = BatchThresholds::new(256, 4 * 1024 * 1024, 128, 128 * 1024).unwrap();
    let stream = StreamLimits::new(
        256,
        16 * 1024,
        4 * 1024 * 1024,
        1024,
        1024 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        64 * 1024,
        64 * 1024,
        4096,
        256,
        64 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
    .with_project_limits(
        ProjectAnalysisLimits::new(1, 64 * 1024, 64 * 1024, 1, 64 * 1024, 256, 256).unwrap(),
    );
    let config = RuntimeConfig::new(
        64 * 1024,
        64 * 1024,
        4096,
        256,
        256,
        1,
        64 * 1024 * 1024,
        ParserSettings::new(4096).unwrap(),
    )
    .unwrap();
    let parser = Arc::new(TreeSitterProvider::new(config).unwrap());
    let bound = GenerationBoundSnapshot::new(&snapshot, &source).unwrap();
    let parse_request = ParseRequest::new(
        bound.clone(),
        LanguageId::new("rust").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        &limits,
    )
    .unwrap();
    let cancellation = Cancellation::new();
    let native = execute_parse(
        parser.as_ref(),
        &parse_request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    )
    .unwrap();
    assert_eq!(
        native.report().coverage().status(),
        CoverageStatus::Complete
    );
    let mut facts = native.facts().to_vec();
    edit(&mut facts);
    let input = ProjectSourceInput::new(
        bound,
        LanguageId::new("rust").unwrap(),
        EncodingId::utf8(),
        false,
        Vec::new(),
    );
    let context = b"{}";
    let build_context = BuildContextIdentity::new(content_hash(b"rust-owner-test"));
    let request = ProjectAnalysisRequest::new(
        AnalysisUnitId::new("owner-unit").unwrap(),
        BuildTargetId::new("//fixture:owner").unwrap(),
        build_context,
        content_hash(context),
        context,
        vec![input],
        AnalysisTier::TierB,
        &limits,
    )
    .unwrap();
    let producer = ProducerIdentity::new(
        "rootlight-owner-test",
        "1.0.0",
        content_hash(b"rust-owner-test"),
    )
    .unwrap();
    let analyzer = SemanticProjectAnalyzer::new(
        SemanticProjectLanguage::Rust,
        parser,
        producer,
        content_hash(b"native-test-binary"),
        build_context,
    )
    .unwrap();
    let parsed = vec![ParsedInput {
        input: &request.inputs()[0],
        facts,
        native_imports: BTreeMap::new(),
        diagnostics: native.diagnostics().to_vec(),
        parse_status: native.report().coverage().status(),
        syntax_nodes: native.report().resources().syntax_nodes(),
        max_syntax_depth: native.report().resources().max_syntax_depth(),
    }];
    inspect(ProjectFactsBuilder::new(
        &analyzer,
        &request,
        parsed,
        &cancellation,
    ));
}

fn with_materialized(
    text: &str,
    edit: impl FnOnce(&mut Vec<SyntaxFact>),
    inspect: impl FnOnce(&mut ProjectFactsBuilder<'_, '_, '_>),
) {
    with_parsed(text, edit, |mut builder| {
        builder.materialize_files_and_provenance().unwrap();
        builder.collect_syntax().unwrap();
        builder.materialize_entities().unwrap();
        inspect(&mut builder);
    });
}

fn entities(records: &[IrRecord], kind: EntityKind, name: &str) -> Vec<EntityRecord> {
    records
        .iter()
        .filter_map(|record| match record {
            IrRecord::Entity(entity) if entity.kind == kind && entity.canonical_name == name => {
                Some(entity.clone())
            }
            _ => None,
        })
        .collect()
}

fn claim(records: &[IrRecord], symbol: SymbolId) -> SymbolIdentityClaim {
    let claims = records
        .iter()
        .filter_map(|record| match record {
            IrRecord::Extension(envelope)
                if envelope.namespace == rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE =>
            {
                let value = decode_symbol_identity_claim_envelope(envelope).unwrap();
                (value.symbol == symbol).then_some(value)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    claims[0].clone()
}

fn assert_owned(
    builder: &ProjectFactsBuilder<'_, '_, '_>,
    method: &EntityRecord,
    owner: &EntityRecord,
) {
    let current = entities(&builder.records, method.kind, &method.canonical_name)
        .into_iter()
        .find(|entity| entity.id == method.id)
        .unwrap();
    assert_eq!(current.container, Some(ContainerRef::Entity(owner.id)));
    let identity = claim(&builder.records, method.id);
    assert_eq!(identity.container, current.container);
    assert_eq!(identity.derived_symbol(), method.id);
    for predicate in [RelationPredicate::Declares, RelationPredicate::Contains] {
        let edges = builder
            .records
            .iter()
            .filter_map(|record| match record {
                IrRecord::Relation(edge)
                    if edge.predicate == predicate
                        && edge.object == RelationEndpoint::Entity(method.id) =>
                {
                    Some(edge)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].subject, RelationEndpoint::Entity(owner.id));
        assert_eq!(edges[0].id, derive_relation_record_id(edges[0]).unwrap());
    }
    let definitions = builder.records.iter().filter_map(|record| match record {
        IrRecord::Occurrence(site) if site.role == OccurrenceRole::Definition
            && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == method.id) => Some(site),
        _ => None,
    }).collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].enclosing, Some(owner.id));
    assert_eq!(
        definitions[0].id,
        derive_occurrence_record_id(definitions[0]).unwrap()
    );
}

#[test]
fn project_inherent_owner_projection_preserves_legacy_symbol_recipes() {
    for text in [
        "pub struct Demo; impl Demo { pub fn answer(&self) {} }",
        "impl Demo { pub fn answer(&self) {} } pub struct Demo;",
    ] {
        with_materialized(
            text,
            |_| {},
            |builder| {
                let method = entities(&builder.records, EntityKind::Method, "answer").remove(0);
                let owner = entities(&builder.records, EntityKind::Struct, "Demo").remove(0);
                let before = claim(&builder.records, method.id);
                let legacy_drafts = builder.occurrences.clone();
                builder.rust_definition_owners = rust_inherent_owners::reconcile(builder).unwrap();
                assert_eq!(builder.rust_definition_owners.len(), 1);
                let after = claim(&builder.records, method.id);
                let mut expected = before;
                expected.container = Some(ContainerRef::Entity(owner.id));
                assert_eq!(after, expected);
                for (old, new) in legacy_drafts.iter().zip(&builder.occurrences) {
                    assert_eq!(old.enclosing_declaration, new.enclosing_declaration);
                    assert_eq!(old.source, new.source);
                }
                builder.materialize_imports_and_occurrences().unwrap();
                assert_owned(builder, &method, &owner);
            },
        );
    }
}

#[test]
fn project_build_routes_proved_owner_through_definition_materialization() {
    with_parsed(
        "struct Demo; impl Demo { fn answer(&self) {} }",
        |_| {},
        |builder| {
            let project = builder.build().unwrap();
            let method = entities(&project.records, EntityKind::Method, "answer").remove(0);
            let owner = entities(&project.records, EntityKind::Struct, "Demo").remove(0);
            assert_eq!(method.container, Some(ContainerRef::Entity(owner.id)));
            assert!(project.records.iter().any(|record| matches!(record,
            IrRecord::Relation(edge) if edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(owner.id)
                && edge.object == RelationEndpoint::Entity(method.id))));
            assert!(project.records.iter().any(|record| matches!(record,
            IrRecord::Occurrence(site) if site.role == OccurrenceRole::Definition
                && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == method.id)
                && site.enclosing == Some(owner.id))));
        },
    );
}

#[test]
fn project_distinct_impl_ids_survive_owner_order_and_unrelated_impl_insertion() {
    let sources = [
        "struct A; struct B; impl A { fn same(&self) {} } impl B { fn same(&self) {} }",
        "impl B { fn same(&self) {} } impl A { fn same(&self) {} } struct B; struct A;",
        "struct C; impl C { fn other(&self) {} } struct B; struct A; impl A { fn same(&self) {} } impl B { fn same(&self) {} }",
    ];
    let mut expected = None;
    for text in sources {
        with_materialized(
            text,
            |_| {},
            |builder| {
                let methods = entities(&builder.records, EntityKind::Method, "same");
                let ids = methods
                    .iter()
                    .map(|method| method.id)
                    .collect::<BTreeSet<_>>();
                assert_eq!(ids.len(), 2);
                if let Some(expected) = &expected {
                    assert_eq!(&ids, expected);
                } else {
                    expected = Some(ids.clone());
                }
                let owners = rust_inherent_owners::reconcile(builder).unwrap();
                for method in &methods {
                    let draft = builder
                        .occurrences
                        .iter()
                        .find(|draft| {
                            draft.role == OccurrenceRole::Definition
                                && draft.enclosing_declaration.and_then(|local| {
                                    builder
                                        .symbol_by_declaration
                                        .get(&(draft.file, local))
                                        .copied()
                                }) == Some(method.id)
                        })
                        .unwrap();
                    assert!(
                        rust_inherent_owners::definition_enclosing(
                            &owners,
                            draft,
                            &OccurrenceTarget::Resolved { symbol: method.id }
                        )
                        .is_some()
                    );
                }
                assert_eq!(
                    entities(&builder.records, EntityKind::Method, "same")
                        .iter()
                        .map(|method| method.id)
                        .collect::<BTreeSet<_>>(),
                    ids
                );
            },
        );
    }
}

#[test]
fn project_owner_projection_preserves_nested_function_identity_and_container() {
    with_materialized(
        "struct Demo; impl Demo { fn answer(&self) { fn child() {} child(); } }",
        |_| {},
        |builder| {
            let child = entities(&builder.records, EntityKind::Function, "child").remove(0);
            let before = claim(&builder.records, child.id);
            let owners = rust_inherent_owners::reconcile(builder).unwrap();
            assert_eq!(owners.len(), 1);
            assert_eq!(
                entities(&builder.records, EntityKind::Function, "child"),
                vec![child.clone()]
            );
            assert_eq!(claim(&builder.records, child.id), before);
        },
    );
}

#[test]
fn project_inherent_owner_projection_preserves_original_deno_method_ranges() {
    let text = include_str!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/rust_named_fields/event_loop.rs"
    );
    assert_eq!(text.len(), 1724);
    with_materialized(
        text,
        |_| {},
        |builder| {
            let owner = entities(&builder.records, EntityKind::Struct, "EventLoopPhases").remove(0);
            let originals = [
                ("run_close_callbacks", 1392, 1411),
                ("drain_v8_close_callbacks", 1608, 1632),
            ]
            .map(|(name, start, end)| {
                (
                    entities(&builder.records, EntityKind::Method, name).remove(0),
                    start,
                    end,
                )
            });
            builder.rust_definition_owners = rust_inherent_owners::reconcile(builder).unwrap();
            builder.materialize_imports_and_occurrences().unwrap();
            for (method, start, end) in originals {
                assert_owned(builder, &method, &owner);
                let site = builder.records.iter().find_map(|record| match record {
                IrRecord::Occurrence(site) if site.role == OccurrenceRole::Definition
                    && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == method.id) => Some(site),
                _ => None,
            }).unwrap();
                assert_eq!(site.source.span().start_byte(), start);
                assert_eq!(site.source.span().end_byte(), end);
            }
        },
    );
}

#[test]
fn project_inherent_owners_use_written_module_identity() {
    let text = "mod one { pub struct Demo; impl Demo { fn same(&self) {} } }\nmod two { impl Demo { fn same(&self) {} } pub struct Demo; }";
    with_materialized(
        text,
        |_| {},
        |builder| {
            let methods = entities(&builder.records, EntityKind::Method, "same");
            let structs = entities(&builder.records, EntityKind::Struct, "Demo");
            assert_eq!(methods.len(), 2);
            assert_ne!(methods[0].id, methods[1].id);
            builder.rust_definition_owners = rust_inherent_owners::reconcile(builder).unwrap();
            assert_eq!(builder.rust_definition_owners.len(), 2);
            builder.materialize_imports_and_occurrences().unwrap();
            for method in methods {
                let owner = structs
                    .iter()
                    .find(|owner| owner.container == method.container)
                    .unwrap();
                assert_owned(builder, &method, owner);
            }
        },
    );
}

#[test]
fn project_inherent_owner_refusal_preserves_the_legacy_graph() {
    for text in [
        "impl Missing { fn answer(&self) {} }",
        "struct Demo; use external::*; impl Demo { fn answer(&self) {} }",
        "struct Demo; type Alias = Demo; impl Alias { fn answer(&self) {} }",
        "struct Demo<T>(T); impl<T> Demo<T> { fn answer(&self) {} }",
        "struct Demo; trait Other { fn answer(&self); } impl Other for Demo { fn answer(&self) {} }",
        "#[cfg(feature = \"conditional\")] struct Demo; impl Demo { fn answer(&self) {} }",
    ] {
        with_materialized(
            text,
            |_| {},
            |builder| {
                let methods = entities(&builder.records, EntityKind::Method, "answer");
                assert!(!methods.is_empty());
                let before = methods
                    .iter()
                    .map(|method| claim(&builder.records, method.id))
                    .collect::<Vec<_>>();
                let owners = rust_inherent_owners::reconcile(builder).unwrap();
                assert!(owners.is_empty(), "unsupported owner context: {text}");
                for (method, identity) in methods.iter().zip(before) {
                    assert_eq!(claim(&builder.records, method.id), identity);
                    assert_eq!(
                        entities(&builder.records, EntityKind::Method, "answer")
                            .iter()
                            .find(|entity| entity.id == method.id)
                            .unwrap(),
                        method
                    );
                    assert!(!builder.records.iter().any(|record| matches!(record,
                    IrRecord::Relation(edge) if edge.predicate == RelationPredicate::Contains
                    && edge.object == RelationEndpoint::Entity(method.id))));
                }
            },
        );
    }
}

#[test]
fn project_owner_reconciliation_refuses_inconsistent_retained_records() {
    for tamper in ["entity", "claim", "declares", "duplicate", "missing-owner"] {
        with_materialized(
            "struct Demo; impl Demo { fn answer(&self) {} }",
            |_| {},
            |builder| {
                let method = entities(&builder.records, EntityKind::Method, "answer").remove(0);
                let owner = entities(&builder.records, EntityKind::Struct, "Demo").remove(0);
                match tamper {
                    "entity" => {
                        for record in &mut builder.records {
                            if let IrRecord::Entity(entity) = record
                                && entity.id == method.id
                            {
                                entity.container = Some(ContainerRef::Entity(owner.id));
                            }
                        }
                    }
                    "claim" => {
                        for record in &mut builder.records {
                            if let IrRecord::Extension(envelope) = record
                                && envelope.namespace
                                    == rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE
                            {
                                let mut identity =
                                    decode_symbol_identity_claim_envelope(envelope).unwrap();
                                if identity.symbol == method.id {
                                    identity.container = Some(ContainerRef::Entity(owner.id));
                                    *envelope = new_symbol_identity_claim_envelope(
                                        &identity,
                                        method.generation,
                                        method.provenance,
                                        method.evidence.source.clone().unwrap(),
                                    )
                                    .unwrap();
                                }
                            }
                        }
                    }
                    "declares" => {
                        for record in &mut builder.records {
                            if let IrRecord::Relation(edge) = record
                                && edge.predicate == RelationPredicate::Declares
                                && edge.object == RelationEndpoint::Entity(method.id)
                            {
                                edge.subject = RelationEndpoint::Entity(owner.id);
                                edge.id = derive_relation_record_id(edge).unwrap();
                            }
                        }
                    }
                    "duplicate" => builder.records.push(IrRecord::Entity(method.clone())),
                    "missing-owner" => builder.records.retain(|record| {
                        !matches!(record,
                    IrRecord::Entity(entity) if entity.id == owner.id)
                    }),
                    _ => unreachable!(),
                }
                let before = builder.records.clone();
                assert!(rust_inherent_owners::reconcile(builder).unwrap().is_empty());
                assert_eq!(&builder.records[..before.len()], before.as_slice());
                assert!(!builder.records.iter().any(|record| matches!(record,
                IrRecord::Relation(edge) if edge.predicate == RelationPredicate::Contains
                && edge.object == RelationEndpoint::Entity(method.id))));
            },
        );
    }
}

#[test]
fn project_missing_impl_header_does_not_recover_a_method_from_a_struct_name() {
    with_materialized(
        "struct Demo; impl Demo { fn answer(&self) {} }",
        |facts| {
            facts.retain(|fact| fact.syntax_kind().as_str() != "rust.impl_type.scope_type");
        },
        |builder| {
            assert!(entities(&builder.records, EntityKind::Method, "answer").is_empty());
            assert!(rust_inherent_owners::reconcile(builder).unwrap().is_empty());
            assert!(entities(&builder.records, EntityKind::Method, "answer").is_empty());
        },
    );
}

#[test]
fn project_definition_owner_projection_keeps_exact_target_authority() {
    with_materialized(
        "struct Demo; impl Demo { fn answer(&self) {} }",
        |_| {},
        |builder| {
            let method = entities(&builder.records, EntityKind::Method, "answer").remove(0);
            let owner = entities(&builder.records, EntityKind::Struct, "Demo").remove(0);
            let proof = rust_inherent_owners::reconcile(builder).unwrap();
            let draft = builder
                .occurrences
                .iter()
                .find(|draft| {
                    draft.role == OccurrenceRole::Definition
                        && draft.spelling_hash == content_hash(b"answer")
                })
                .unwrap()
                .clone();
            assert_eq!(
                rust_inherent_owners::definition_enclosing(
                    &proof,
                    &draft,
                    &OccurrenceTarget::Resolved { symbol: method.id }
                ),
                Some(owner.id)
            );
            assert_eq!(
                rust_inherent_owners::definition_enclosing(
                    &proof,
                    &draft,
                    &OccurrenceTarget::Resolved { symbol: owner.id }
                ),
                None
            );
            let mut reference = draft.clone();
            reference.role = OccurrenceRole::Reference;
            assert_eq!(
                rust_inherent_owners::definition_enclosing(
                    &proof,
                    &reference,
                    &OccurrenceTarget::Resolved { symbol: method.id }
                ),
                None
            );
            let mut foreign = draft;
            foreign.source = SourceRef::new(
                foreign.source.repository(),
                foreign.source.generation(),
                foreign.source.span(),
                content_hash(b"different source frame"),
                None,
            );
            assert_eq!(
                rust_inherent_owners::definition_enclosing(
                    &proof,
                    &foreign,
                    &OccurrenceTarget::Resolved { symbol: method.id }
                ),
                None
            );
        },
    );
}

#[test]
fn project_source_above_identity_signature_bound_still_projects_plain_owners() {
    let text = format!(
        "struct Demo; impl Demo {{ fn answer(&self) {{}} }} /*{}*/",
        "padding ".repeat(600)
    );
    assert!(text.len() > rootlight_ir::MAX_LEXICAL_SIGNATURE_BYTES);
    with_materialized(
        &text,
        |_| {},
        |builder| {
            let method = entities(&builder.records, EntityKind::Method, "answer").remove(0);
            let identity = claim(&builder.records, method.id);
            let owner = entities(&builder.records, EntityKind::Struct, "Demo").remove(0);
            builder.rust_definition_owners = rust_inherent_owners::reconcile(builder).unwrap();
            assert_eq!(builder.rust_definition_owners.len(), 1);
            let mut expected = identity;
            expected.container = Some(ContainerRef::Entity(owner.id));
            assert_eq!(claim(&builder.records, method.id), expected);
            builder.materialize_imports_and_occurrences().unwrap();
            assert_owned(builder, &method, &owner);
        },
    );
}

#[test]
fn project_owner_projection_refuses_an_incomplete_parse() {
    with_materialized(
        "struct Demo; impl Demo { fn answer(&self) {} }",
        |_| {},
        |builder| {
            builder.parsed[0].parse_status = CoverageStatus::Bounded;
            let before = builder.records.clone();
            assert!(rust_inherent_owners::reconcile(builder).unwrap().is_empty());
            assert_eq!(builder.records, before);
        },
    );
}

#[test]
fn project_direct_field_join_preserves_authoritative_rows_and_exact_definition_target() {
    with_materialized(
        "pub struct Demo { pub value: Vec<u8> }",
        |_| {},
        |builder| {
            let field = entities(&builder.records, EntityKind::Field, "value").remove(0);
            let owner = entities(&builder.records, EntityKind::Struct, "Demo").remove(0);
            let identity = claim(&builder.records, field.id);
            assert_eq!(field.container, Some(ContainerRef::Entity(owner.id)));
            let original_rows = builder
                .records
                .iter()
                .filter(|record| {
                    matches!(
                        record,
                        IrRecord::Entity(_) | IrRecord::Extension(_) | IrRecord::Relation(_)
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            builder.rust_definition_owners = rust_inherent_owners::reconcile(builder).unwrap();
            assert_eq!(builder.rust_definition_owners.len(), 1);
            assert_eq!(
                entities(&builder.records, EntityKind::Field, "value").as_slice(),
                std::slice::from_ref(&field)
            );
            assert_eq!(claim(&builder.records, field.id), identity);
            for row in original_rows {
                assert!(builder.records.contains(&row));
            }
            builder.materialize_imports_and_occurrences().unwrap();
            assert_owned(builder, &field, &owner);
        },
    );
}

#[test]
fn project_field_join_refuses_ambiguous_or_inconsistent_authoritative_containers() {
    for tamper in [
        "entity",
        "claim",
        "definition",
        "duplicate",
        "existing-contains",
    ] {
        with_materialized(
            "struct Demo { value: u8 }",
            |_| {},
            |builder| {
                let field = entities(&builder.records, EntityKind::Field, "value").remove(0);
                match tamper {
                    "entity" => {
                        for record in &mut builder.records {
                            if let IrRecord::Entity(entity) = record
                                && entity.id == field.id
                            {
                                entity.container = Some(ContainerRef::File(
                                    field.evidence.source.as_ref().unwrap().span().file(),
                                ));
                            }
                        }
                    }
                    "claim" => {
                        for record in &mut builder.records {
                            if let IrRecord::Extension(envelope) = record
                                && envelope.namespace
                                    == rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE
                            {
                                let mut claim =
                                    decode_symbol_identity_claim_envelope(envelope).unwrap();
                                if claim.symbol == field.id {
                                    claim.container = Some(ContainerRef::File(
                                        field.evidence.source.as_ref().unwrap().span().file(),
                                    ));
                                    *envelope = new_symbol_identity_claim_envelope(
                                        &claim,
                                        field.generation,
                                        field.provenance,
                                        field.evidence.source.clone().unwrap(),
                                    )
                                    .unwrap();
                                }
                            }
                        }
                    }
                    "definition" => builder.occurrences.retain(|draft| {
                        draft.name != "value" || draft.role != OccurrenceRole::Definition
                    }),
                    "duplicate" => builder.records.push(IrRecord::Entity(field.clone())),
                    "existing-contains" => {
                        let mut edge = builder
                            .records
                            .iter()
                            .find_map(|record| match record {
                                IrRecord::Relation(edge)
                                    if edge.predicate == RelationPredicate::Declares
                                        && edge.object == RelationEndpoint::Entity(field.id) =>
                                {
                                    Some(edge.clone())
                                }
                                _ => None,
                            })
                            .unwrap();
                        edge.predicate = RelationPredicate::Contains;
                        edge.id = derive_relation_record_id(&edge).unwrap();
                        builder.records.push(IrRecord::Relation(edge));
                    }
                    _ => unreachable!(),
                }
                let before = builder.records.clone();
                assert!(rust_inherent_owners::reconcile(builder).unwrap().is_empty());
                assert_eq!(&builder.records[..before.len()], before.as_slice());
            },
        );
    }
}

#[test]
fn project_forged_field_inside_function_pointer_type_keeps_legacy_graph() {
    let text = "struct Demo { callback: fn(value: u8) }";
    with_materialized(
        text,
        |facts| {
            let field = facts
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == "rust.field.declaration")
                .unwrap()
                .local_id();
            let definition = facts
                .iter()
                .find(|fact| {
                    fact.syntax_kind().as_str() == "rust.field_identifier.definition"
                        && fact.parent() == Some(field)
                })
                .unwrap()
                .local_id();
            let parents = facts
                .iter()
                .map(|fact| (fact.local_id(), fact.parent()))
                .collect::<BTreeMap<_, _>>();
            // Remove the original type subtree rather than leave an invalid child
            // outside the forged Field span. The source proof must reject a valid graph.
            facts.retain(|fact| {
                if [field, definition].contains(&fact.local_id()) {
                    return true;
                }
                let mut parent = fact.parent();
                while let Some(local) = parent {
                    if local == field {
                        return false;
                    }
                    parent = parents[&local];
                }
                true
            });
            let start = text.find("value: u8").unwrap() as u64;
            for fact in facts.iter_mut() {
                let replacement = if fact.local_id() == field {
                    Some((start, start + "value: u8".len() as u64))
                } else if fact.syntax_kind().as_str() == "rust.field_identifier.definition"
                    && fact.parent() == Some(field)
                {
                    Some((start, start + "value".len() as u64))
                } else {
                    None
                };
                if let Some((start, end)) = replacement {
                    *fact = SyntaxFact::new(
                        fact.local_id(),
                        fact.parent(),
                        fact.kind(),
                        SourceSpan::new(fact.span().file(), start, end).unwrap(),
                        fact.depth(),
                        fact.syntax_kind().clone(),
                    );
                }
            }
            for fact in facts.iter() {
                if let Some(parent) = fact.parent() {
                    let parent = facts
                        .iter()
                        .find(|candidate| candidate.local_id() == parent)
                        .unwrap();
                    assert_eq!(parent.depth().checked_add(1), Some(fact.depth()));
                    assert!(parent.span().start_byte() <= fact.span().start_byte());
                    assert!(fact.span().end_byte() <= parent.span().end_byte());
                }
            }
        },
        |builder| {
            let field = entities(&builder.records, EntityKind::Field, "value").remove(0);
            let before = builder.records.clone();
            assert!(rust_inherent_owners::reconcile(builder).unwrap().is_empty());
            assert_eq!(&builder.records[..before.len()], before.as_slice());
            assert!(!builder.records.iter().any(|record| matches!(record,
            IrRecord::Relation(edge) if edge.predicate == RelationPredicate::Contains
                && edge.object == RelationEndpoint::Entity(field.id))));
        },
    );
}

#[test]
fn project_duplicate_written_field_names_never_gain_contains_projection() {
    with_materialized(
        "struct Demo { value: u8, value: u16 }",
        |_| {},
        |builder| {
            let before = builder.records.clone();
            assert!(rust_inherent_owners::reconcile(builder).unwrap().is_empty());
            assert_eq!(&builder.records[..before.len()], before.as_slice());
        },
    );
}

#[test]
fn project_service_unbounded_syntax_policy_preserves_original_deno_owners_and_provenance() {
    let text = include_str!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/rust_named_fields/event_loop.rs"
    );
    assert_eq!(text.len(), 1724);
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    std::fs::write(temporary.path().join("event_loop.rs"), text).unwrap();
    let repository: RepositoryId = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
    let generation: GenerationId = "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
        .parse()
        .unwrap();
    let root = RepositoryRoot::open(repository, temporary.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("event_loop.rs")).unwrap(),
            64 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let context = b"{}";
    let build_context = BuildContextIdentity::new(content_hash(b"service-rust-owner-work-credit"));
    let producer = ProducerIdentity::new(
        "rootlight-rust-project-test",
        "1.0.0",
        content_hash(context),
    )
    .unwrap();
    let binary = content_hash(b"source-backed-rust-inherent-owner-project-test");
    let mut baseline: Option<rootlight_ir::NormalizedIrDocument> = None;
    for (maximum_source_bytes, maximum_syntax_nodes) in [
        (64 * 1024, 4096),
        (64 * 1024, usize::MAX),
        (64 * 1024 * 1024, usize::MAX),
    ] {
        // The finite profile fits both pointer widths. The next profiles use the
        // service helper and public source ceilings with its unbounded node policy.
        let batch = BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).unwrap();
        let stream = StreamLimits::new(
            512 * 1024 * 1024,
            512 * 1024 * 1024,
            512 * 1024 * 1024,
            16_384,
            16 * 1024 * 1024,
            128 * 1024 * 1024,
            batch,
        )
        .unwrap();
        let limits = AnalysisLimits::new(
            maximum_source_bytes,
            maximum_syntax_nodes,
            4096,
            32,
            512 * 1024 * 1024,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .unwrap()
        .with_project_limits(
            ProjectAnalysisLimits::new(1, 256 * 1024, 64 * 1024, 16, 128 * 1024, 128, 128).unwrap(),
        );
        let parser = Arc::new(
            TreeSitterProvider::new(
                RuntimeConfig::new(
                    maximum_source_bytes,
                    maximum_syntax_nodes,
                    4096,
                    32,
                    64,
                    1,
                    16 * 1024 * 1024,
                    ParserSettings::new(4096).unwrap(),
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let analyzer = SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::Rust,
            parser,
            producer.clone(),
            binary,
            build_context,
        )
        .unwrap();
        let input = ProjectSourceInput::new(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("rust").unwrap(),
            EncodingId::utf8(),
            false,
            Vec::new(),
        );
        let request = ProjectAnalysisRequest::new(
            AnalysisUnitId::new("fixture.rust-owner").unwrap(),
            BuildTargetId::new("//fixture:rust-owner").unwrap(),
            build_context,
            content_hash(context),
            context,
            vec![input],
            AnalysisTier::TierB,
            &limits,
        )
        .unwrap();
        let output = rootlight_adapter_sdk::execute_project_analysis(
            &analyzer,
            &request,
            rootlight_ir::ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .expect("service-equivalent work credit must admit the real Deno project");
        let document = output.document();
        assert_eq!(document.repository, repository);
        assert_eq!(document.generation, generation);
        assert_eq!(document.files.len(), 1);
        assert_eq!(document.files[0].content_hash, source.content_hash());
        assert!(!document.provenance.is_empty());
        assert!(document.provenance.iter().all(|record| {
            record.producer == producer
                && record.binary_digest == binary
                && record.tier == AnalysisTier::TierB
                && record.build_context == build_context
        }));
        let owners = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Struct && entity.canonical_name == "EventLoopPhases"
            })
            .collect::<Vec<_>>();
        assert_eq!(owners.len(), 1);
        let owner = owners[0];
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.source.span().start_byte() == 1104
                        && site.source.span().end_byte() == 1119
                        && site.target == OccurrenceTarget::Resolved { symbol: owner.id }
                })
                .count(),
            1
        );
        for (name, kind, start, end) in [
            ("close_callbacks", EntityKind::Field, 1160, 1175),
            ("run_close_callbacks", EntityKind::Method, 1392, 1411),
            ("drain_v8_close_callbacks", EntityKind::Method, 1608, 1632),
        ] {
            let members = document
                .entities
                .iter()
                .filter(|entity| entity.kind == kind && entity.canonical_name == name)
                .collect::<Vec<_>>();
            assert_eq!(members.len(), 1, "{name}");
            let member = members[0];
            assert_eq!(member.container, Some(ContainerRef::Entity(owner.id)));
            let claims = document
                .extensions
                .iter()
                .filter(|extension| {
                    extension.namespace == rootlight_ir::SYMBOL_IDENTITY_CLAIM_NAMESPACE
                })
                .map(|extension| decode_symbol_identity_claim_envelope(extension).unwrap())
                .filter(|claim| claim.symbol == member.id)
                .collect::<Vec<_>>();
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0].derived_symbol(), member.id);
            assert_eq!(claims[0].container, member.container);
            assert_eq!(
                claims[0].build_context_discriminator.as_slice(),
                build_context.digest().as_bytes()
            );
            let definitions = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: member.id }
                })
                .collect::<Vec<_>>();
            assert_eq!(definitions.len(), 1);
            let site = definitions[0];
            assert_eq!(site.source.span().start_byte(), start);
            assert_eq!(site.source.span().end_byte(), end);
            assert_eq!(site.enclosing, Some(owner.id));
            assert_eq!(site.provenance, member.provenance);
            assert_eq!(site.source.content_hash(), source.content_hash());
            assert_eq!(
                &text.as_bytes()[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
                name.as_bytes()
            );
            let contains = document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::Contains
                        && edge.object == RelationEndpoint::Entity(member.id)
                })
                .collect::<Vec<_>>();
            assert_eq!(contains.len(), 1);
            assert_eq!(contains[0].subject, RelationEndpoint::Entity(owner.id));
            assert_eq!(contains[0].provenance, member.provenance);
            assert_eq!(
                contains[0].id,
                derive_relation_record_id(contains[0]).unwrap()
            );
            let evidence = contains[0].evidence.source.as_ref().unwrap();
            assert_eq!(evidence.content_hash(), source.content_hash());
            assert!(evidence.span().start_byte() <= start && end <= evidence.span().end_byte());
        }
        assert!(!document.occurrences.iter().any(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 1455
                && site.source.span().end_byte() == 1470
        }));
        if let Some(expected) = &baseline {
            assert_eq!(
                document, expected,
                "work credit must not change project identities or facts"
            );
        } else {
            baseline = Some(document.clone());
        }
    }
}
