//! Coq native declarations retain authored identities and original byte ranges.
//! These controls deliberately leave kernel checking and flat module owners open.

use super::*;
use rootlight_ir::ContainerRef;

const COQ: LanguageCase = LanguageCase {
    name: "coq",
    path: "src/written.v",
    frontend: "tree-sitter-rocq-0.2.0",
    source: "Definition value := 0.\n",
    generated: false,
    body_before: "0",
    body_after: "1",
};

fn record_field<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    name: &str,
) -> &'a rootlight_ir::EntityRecord {
    let fields: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Field && entity.canonical_name == name)
        .collect();
    assert_eq!(fields.len(), 1, "{name}: {:?}", document.entities);
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.target
                == OccurrenceTarget::Resolved {
                    symbol: fields[0].id,
                }
        })
        .collect();
    assert_eq!(sites.len(), 1, "{name}");
    assert_eq!(sites[0].role, OccurrenceRole::Declaration);
    fields[0]
}

#[test]
fn coq_record_class_original_fields_and_constructors_keep_physical_owners() {
    for (source, name, kind, constructor, fields) in [
        (
            include_str!("../fixtures/coq-records/binder_annot.v"),
            "binder_annot",
            EntityKind::Struct,
            Some("mkBindAnn"),
            vec!["binder_name", "binder_relevance"],
        ),
        (
            include_str!("../fixtures/coq-records/checker_flags.v"),
            "checker_flags",
            EntityKind::Class,
            None,
            vec![
                "check_univs",
                "prop_sub_type",
                "indices_matter",
                "lets_in_constructor_types",
            ],
        ),
        (
            include_str!("../fixtures/coq-records/retroknowledge.v"),
            "t",
            EntityKind::Struct,
            Some("mk_retroknowledge"),
            vec![
                "retro_int63",
                "retro_float64",
                "retro_string",
                "retro_array",
            ],
        ),
        (
            include_str!("../fixtures/coq-records/listable.v"),
            "Listable",
            EntityKind::Class,
            None,
            vec!["list_all", "find_index", "cardinality"],
        ),
        (
            include_str!("../fixtures/coq-records/dependent_sigma.v"),
            "Sigma",
            EntityKind::Struct,
            None,
            vec!["fst", "snd"],
        ),
    ] {
        let result = output(source);
        let document = result.document();
        named_definition(document, name, kind);
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.kind == kind && entity.canonical_name == name)
            .unwrap();
        for field in fields {
            let member = record_field(document, field);
            assert_eq!(
                member.container,
                Some(ContainerRef::Entity(owner.id)),
                "{field}"
            );
            assert!(
                document
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(owner.id)
                        && edge.object == RelationEndpoint::Entity(member.id))
            );
        }
        let constructors: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Constructor)
            .collect();
        assert_eq!(constructors.len(), usize::from(constructor.is_some()));
        if let Some(name) = constructor {
            named_definition(document, name, EntityKind::Constructor);
            assert_eq!(
                constructors[0].container,
                Some(ContainerRef::Entity(owner.id))
            );
        }
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "check_guard"
                    || entity.canonical_name.starts_with("Build_"))
        );
        assert!(document.skipped_regions.iter().any(|gap| gap.detail
            == "coq-record-class-elaboration-projection-generation-and-field-binding-unavailable"));
    }
}

#[test]
fn coq_record_class_singleton_and_structure_do_not_invent_generated_entities() {
    let singleton = include_str!("../fixtures/coq-records/debug_opt.v");
    let result = output(singleton);
    named_definition(result.document(), "debug_opt", EntityKind::Class);
    record_field(result.document(), "debug");
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Constructor)
    );
    let source = "Structure Pair := make_pair { left : nat; right : nat }.\nRecord Anonymous := { _ : nat; _value : nat }.\nDefinition made := {| left := 0; right := 1 |}.\n";
    let result = output(source);
    named_definition(result.document(), "Pair", EntityKind::Struct);
    named_definition(result.document(), "make_pair", EntityKind::Constructor);
    named_definition(result.document(), "Anonymous", EntityKind::Struct);
    for name in ["left", "right", "_value"] {
        record_field(result.document(), name);
    }
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "_"
                || entity.canonical_name.starts_with("Build_"))
    );
}

#[test]
fn coq_record_class_attributes_recovery_and_decoys_keep_applicable_gaps() {
    let source = "(* Record comment_fake := { fake : nat }. *)\nDefinition text := \"Class string_fake := { fake : nat }.\".\nFail Record failed := { failed_field : nat }.\n#[local] Class attributed := { attributed_field : nat }.\nRecord nested := { #[canonical] nested_field : nat }.\nRecord _ := { anonymous_head_field : nat }.\nRecord broken := { broken_field : }.\nDefinition tail := 0.\n";
    let result = output(source);
    named_definition(result.document(), "tail", EntityKind::Constant);
    for name in [
        "comment_fake",
        "string_fake",
        "fake",
        "failed",
        "failed_field",
        "attributed",
        "attributed_field",
        "nested",
        "nested_field",
        "_",
        "anonymous_head_field",
        "broken",
        "broken_field",
    ] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "{name}"
        );
    }
    assert!(
        result.document().skipped_regions.iter().any(
            |gap| gap.detail == "coq-fail-attribute-or-selector-declaration-effect-unavailable"
        )
    );
    let attributed = output(include_str!("../fixtures/coq-records/sigma.v"));
    assert!(
        !attributed.document().entities.iter().any(|entity| matches!(
            entity.kind,
            EntityKind::Struct | EntityKind::Field | EntityKind::Constructor
        ))
    );
    assert!(
        attributed.document().skipped_regions.iter().any(
            |gap| gap.detail == "coq-fail-attribute-or-selector-declaration-effect-unavailable"
        )
    );
}

#[test]
fn coq_record_class_encoded_names_keep_original_sources_and_unresolved_values() {
    let text = "(* λ😀 *)\nRecord résumé := crée { valeur : nat := source_value }.\nClass unique := projection : nat.\n";
    for (encoding, bom) in [
        ("utf-8", false),
        ("utf-8", true),
        ("utf-16le", true),
        ("utf-16be", true),
    ] {
        let source = format!("{}{text}", if bom { "\u{feff}" } else { "" });
        let encode = |text: &str| -> Vec<u8> {
            if encoding == "utf-8" {
                text.as_bytes().to_vec()
            } else {
                text.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            }
        };
        let raw = encode(&source);
        let provider = Arc::new(provider());
        let fixture = Fixture::new(COQ, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(COQ),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, COQ),
            &request,
            &ExtensionSupport::default(),
        );
        validate(&result, &raw, encoding);
        for (name, kind, role) in [
            ("résumé", EntityKind::Struct, OccurrenceRole::Definition),
            ("crée", EntityKind::Constructor, OccurrenceRole::Definition),
            ("valeur", EntityKind::Field, OccurrenceRole::Declaration),
            ("unique", EntityKind::Class, OccurrenceRole::Definition),
            ("projection", EntityKind::Field, OccurrenceRole::Declaration),
        ] {
            let entity = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name && entity.kind == kind)
                .unwrap();
            let sites: Vec<_> = result
                .document()
                .occurrences
                .iter()
                .filter(|site| site.target == OccurrenceTarget::Resolved { symbol: entity.id })
                .collect();
            assert_eq!(sites.len(), 1, "{encoding}/{bom}: {name}");
            let site = sites[0];
            let start = encode(&source[..source.find(name).unwrap()]).len();
            assert_eq!(site.role, role);
            assert_eq!(
                site.source.span().start_byte(),
                u64::try_from(start).unwrap()
            );
            assert_eq!(
                site.source.span().end_byte(),
                u64::try_from(start + encode(name).len()).unwrap()
            );
            assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
            assert_eq!(
                entity.evidence.source.as_ref().unwrap().content_hash(),
                content_hash(&raw)
            );
            assert_eq!(
                result
                    .document()
                    .provenance
                    .iter()
                    .find(|record| record.id == entity.provenance)
                    .unwrap()
                    .tier,
                AnalysisTier::TierD
            );
        }
        let start = encode(&source[..source.find("source_value").unwrap()]).len();
        let uses: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.role == OccurrenceRole::Reference
            })
            .collect();
        assert_eq!(uses.len(), 1, "{encoding}/{bom}");
        assert!(matches!(
            uses[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert_eq!(
            uses[0].source.span().end_byte(),
            u64::try_from(start + encode("source_value").len()).unwrap()
        );
    }
}

#[test]
fn coq_record_class_custom_types_and_field_dependencies_keep_explicit_uncertainty() {
    let source = "Definition chosen := 0.\nRecord Values := { chosen_field : nat; value : nat := chosen_field; method (input : nat) : nat := input }.\nRecord Ambiguous := { unknown_field : nat | 3 }.\nClass Ranked := { ranked : C | 2; default := chosen }.\n";
    let result = output(source);
    let document = result.document();
    for name in [
        "chosen_field",
        "value",
        "method",
        "unknown_field",
        "ranked",
        "default",
    ] {
        record_field(document, name);
    }
    for spelling in ["chosen_field", "chosen", "input"] {
        let at = source.rfind(spelling).unwrap();
        let uses: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == u64::try_from(at).unwrap()
            })
            .collect();
        assert_eq!(uses.len(), 1, "{spelling}");
        assert!(matches!(
            uses[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
    let start = source.find("unknown_field").unwrap();
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "coq-custom-notation-or-generic-tactic-structure-unavailable"
        && gap.source.span().start_byte() == u64::try_from(start).unwrap()));
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "coq-local-term-binder-projection-and-generated-declaration-coverage-incomplete"));
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "input")
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name.starts_with("Build_"))
    );
}

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    output_with_provider(source, &provider)
}

fn output_with_provider(source: &str, provider: &Arc<TreeSitterProvider>) -> AnalysisOutput {
    let fixture = Fixture::new(COQ, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(provider, COQ),
        &request(&fixture.snapshot, &fixture.source, COQ, &budget),
        &ExtensionSupport::default(),
    );
    validate(&result, source.as_bytes(), "utf-8");
    result
}

fn validate(result: &AnalysisOutput, source: &[u8], encoding: &str) {
    let document = result.document();
    validate_ir_document(document, limits().ir(), &ExtensionSupport::default()).unwrap();
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "coq-elaboration-typechecking-proofchecking-and-term-binding-unavailable"));
    for occurrence in &document.occurrences {
        let span = occurrence.source.span();
        let bytes = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        let spelling = if encoding == "utf-8" {
            std::str::from_utf8(bytes).unwrap().to_owned()
        } else {
            assert_eq!(bytes.len() % 2, 0);
            String::from_utf16(
                &bytes
                    .chunks_exact(2)
                    .map(|unit| {
                        let unit = [unit[0], unit[1]];
                        if encoding == "utf-16le" {
                            u16::from_le_bytes(unit)
                        } else {
                            u16::from_be_bytes(unit)
                        }
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(spelling.as_bytes()),
            "{encoding}: {}",
            occurrence.syntax_kind
        );
        assert_eq!(occurrence.source.content_hash(), content_hash(source));
        assert_eq!(occurrence.source.generation(), document.generation);
        assert_eq!(occurrence.source.repository(), document.repository);
        assert_ne!(occurrence.role, OccurrenceRole::CallSite);
        if occurrence.role == OccurrenceRole::Declaration {
            assert!(matches!(
                occurrence.syntax_kind.as_str(),
                "coq.declared_instance_name.definition"
                    | "coq.record_field_name.definition"
                    | "coq.class_field_name.definition"
            ));
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Resolved { .. }
            ));
        } else if occurrence.role != OccurrenceRole::Definition {
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
    }
}

fn named_definition<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
) -> &'a rootlight_ir::OccurrenceRecord {
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == name && entity.kind == kind)
        .unwrap_or_else(|| {
            panic!(
                "missing {name}: {:?}; gaps {:?}",
                document.entities, document.skipped_regions
            )
        });
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
        })
        .collect();
    assert_eq!(definitions.len(), 1, "{name}");
    definitions[0]
}

#[test]
fn coq_core_names_and_inductive_constructor_owners_are_source_bound() {
    let source = "(* marker λ😀 *)\nDefinition résumé := 0.\nTheorem truth : True. Proof. exact I. Qed.\nFixpoint identity (x : nat) : nat := x.\nInductive flag := On | Off.\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("résumé", EntityKind::Constant),
        ("truth", EntityKind::Constant),
        ("identity", EntityKind::Constant),
        ("flag", EntityKind::Enum),
        ("On", EntityKind::Constructor),
        ("Off", EntityKind::Constructor),
    ] {
        let occurrence = named_definition(document, name, kind);
        let span = occurrence.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
    }
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "flag")
        .unwrap();
    for name in ["On", "Off"] {
        let child = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner.id)
                    && relation.object == RelationEndpoint::Entity(child.id))
        );
    }
    assert!(
        document
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Reference
                && occurrence.syntax_kind == "coq.written_reference.reference")
    );
}

#[test]
fn coq_fail_attributes_and_literal_decoys_never_become_global_definitions() {
    let source = "(* Definition comment_fake := 0. (* nested *) *)\nDefinition text := \"Definition string_fake := 0.\".\nFail Definition failed := 0.\n#[local] Definition attributed := 0.\nContext (A : Type).\nDefinition tail := 0.\n";
    let result = output(source);
    named_definition(result.document(), "text", EntityKind::Constant);
    named_definition(result.document(), "tail", EntityKind::Constant);
    for name in ["comment_fake", "string_fake", "failed", "attributed", "A"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "invented {name}"
        );
    }
    for gap in [
        "coq-fail-attribute-or-selector-declaration-effect-unavailable",
        "coq-context-and-assumption-binder-lifetime-unavailable",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|region| region.detail == gap)
        );
    }
}

#[test]
fn coq_flat_module_headers_do_not_invent_namespace_containment() {
    let source =
        "Module A.\nDefinition value := 0.\nEnd A.\nModule B.\nDefinition value := 1.\nEnd B.\n";
    let result = output(source);
    let document = result.document();
    for name in ["A", "B"] {
        named_definition(document, name, EntityKind::Module);
        let module = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.subject == RelationEndpoint::Entity(module.id))
        );
    }
    let values: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "value")
        .collect();
    assert_eq!(values.len(), 2);
    assert_ne!(values[0].id, values[1].id);
    assert!(document.skipped_regions.iter().any(|gap|gap.detail=="coq-module-section-stack-and-qualified-ownership-unavailable"));
}

#[test]
fn coq_recovery_and_generic_sentences_keep_explicit_applicable_gaps() {
    for source in [
        "Definition broken := .\n",
        "Context {term : Type@{i}}.\n",
        "MetaRocq Quote Definition quoted := nat.\n",
    ] {
        let result = output(source);
        assert!(
            result.document().skipped_regions.iter().any(|gap| matches!(
                gap.detail.as_str(),
                "coq-parser-recovery-structure-unavailable"
                    | "coq-custom-notation-or-generic-tactic-structure-unavailable"
                    | "coq-vernacular-construct-structure-unavailable"
                    | "coq-context-and-assumption-binder-lifetime-unavailable"
            )),
            "missing applicable gap for {source}: {:?}",
            result.document().skipped_regions
        );
    }
    let record = output("Record carrier := { field : nat }.\n");
    named_definition(record.document(), "carrier", EntityKind::Struct);
    record_field(record.document(), "field");
    assert!(record.document().skipped_regions.iter().any(|gap| gap.detail == "coq-record-class-elaboration-projection-generation-and-field-binding-unavailable"));
}

#[test]
fn coq_body_changes_preserve_written_constant_identity() {
    let before = output(COQ.source);
    let after = output("Definition value := 1.\n");
    let entity = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "value")
            .unwrap()
            .id
    };
    assert_eq!(entity(&before), entity(&after));
}

#[test]
fn coq_reserved_lone_underscore_is_not_a_declaration_name() {
    for name in ["_", "Type", "Definition", "match"] {
        let source = format!(
            "Definition {name} := 0.\nModule {name}.\nEnd {name}.\nDefinition _value := 0.\n"
        );
        let result = output(&source);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
        named_definition(result.document(), "_value", EntityKind::Constant);
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && gap.detail == "coq-declaration-name-unavailable")
        );
    }
}

#[test]
fn coq_term_and_functor_binders_are_explicit_entity_gaps_and_never_fake_reads() {
    let source = "Definition identity (x : nat) : nat := x.\nModule M (X : S).\nEnd M.\n";
    let result = output(source);
    let document = result.document();
    for name in ["x", "X"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == domain
                    && gap.detail
                        == "coq-term-and-module-binder-entities-and-lifetime-unavailable")
        );
    }
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect();
    let body_x = source.find(":= x").unwrap() + 3;
    assert!(
        references.iter().any(
            |occurrence| occurrence.source.span().start_byte() == body_x as u64
                && occurrence.source.span().end_byte() == body_x as u64 + 1
        )
    );
    for declaration_start in [
        source.find("identity").unwrap(),
        source.find("(x").unwrap() + 1,
        source.find("(X").unwrap() + 1,
        source.find("Module M").unwrap() + 7,
    ] {
        assert!(!references.iter().any(|occurrence| occurrence.source.span().start_byte() == declaration_start as u64));
    }
}

#[test]
fn coq_local_let_and_match_bindings_remain_explicitly_incomplete() {
    let source = "Definition local := let x := 0 in x.\nDefinition choose (n : nat) : nat := match n with | O => 0 | S y => y end.\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for name in ["local", "choose"] {
        named_definition(document, name, EntityKind::Constant);
    }
    for name in ["x", "y", "n"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
    assert!(document.skipped_regions.iter().any(|gap| gap.domain == FactDomain::Entities && gap.detail == "coq-local-term-binder-projection-and-generated-declaration-coverage-incomplete"));
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(
            result
                .report()
                .coverage()
                .domains()
                .iter()
                .any(|coverage| coverage.domain() == domain
                    && coverage.status() != CoverageStatus::Complete)
        );
    }
}

#[test]
fn coq_original_error_root_keeps_file_source_and_does_not_poison_clean_neighbors() {
    let source = include_str!("../fixtures/coq-recovery/MRListable.v");
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rocq::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert_eq!(tree.root_node().kind(), "ERROR");
    let provider = Arc::new(provider());
    let result = output_with_provider(source, &provider);
    let document = result.document();
    assert_eq!(document.files.len(), 1);
    assert_eq!(
        document.files[0].content_hash,
        content_hash(source.as_bytes())
    );
    assert_eq!(document.files[0].byte_length, source.len() as u64);
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "coq-parser-recovery-structure-unavailable")
    );
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.domain == domain
                && gap.detail == "coq-parser-recovery-structure-unavailable"
                && gap.source.span().start_byte() == 0
                && gap.source.span().end_byte() == source.len() as u64
        }));
    }
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    let neighbor = output_with_provider("Definition neighbor := 0.\n", &provider);
    named_definition(neighbor.document(), "neighbor", EntityKind::Constant);
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn coq_utf8_and_utf16_names_use_exact_original_physical_bytes() {
    let source = "\u{feff}(* λ😀 *)\r\nDefinition résumé := 0.\r\nInstance fourni : C := value.\r\nDeclare Instance requis : C.\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| -> Vec<u8> {
            if encoding == "utf-8" {
                text.as_bytes().to_vec()
            } else {
                text.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            }
        };
        let raw = encode(source);
        let provider = Arc::new(provider());
        let fixture = Fixture::new(COQ, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(COQ),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, COQ),
            &request,
            &ExtensionSupport::default(),
        );
        validate(&result, &raw, encoding);
        let occurrence = named_definition(result.document(), "résumé", EntityKind::Constant);
        let start = encode(&source[..source.find("résumé").unwrap()]).len();
        assert_eq!(
            occurrence.source.span().start_byte(),
            start as u64,
            "{encoding}"
        );
        assert_eq!(
            occurrence.source.span().end_byte(),
            (start + encode("résumé").len()) as u64,
            "{encoding}"
        );
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash("résumé".as_bytes())
        );
        for (name, role) in [
            ("fourni", OccurrenceRole::Definition),
            ("requis", OccurrenceRole::Declaration),
        ] {
            let entity = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name && entity.kind == EntityKind::Constant)
                .unwrap();
            let occurrence = result
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .unwrap();
            assert_eq!(occurrence.role, role);
            let start = encode(&source[..source.find(name).unwrap()]).len();
            assert_eq!(occurrence.source.span().start_byte(), start as u64);
            assert_eq!(
                occurrence.source.span().end_byte(),
                (start + encode(name).len()) as u64
            );
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(name.as_bytes())
            );
        }
    }
}

#[test]
fn coq_named_instances_and_declared_requirements_keep_distinct_written_roles() {
    let source = "(* marker λ😀 *)\nInstance provided (input : nat) : C := value.\nInstance methods : C := { field (item : nat) := body }.\nInstance pending : C.\nModule Type Contract.\nDeclare Instance required : C | 2.\nEnd Contract.\nExisting Instance provided.\nExisting Instances provided Module.prior.\nExisting Class C.\n";
    let result = output(source);
    let document = result.document();
    for name in ["provided", "methods", "pending"] {
        let occurrence = named_definition(document, name, EntityKind::Constant);
        let start = source.find(name).unwrap();
        assert_eq!(occurrence.source.span().start_byte(), start as u64);
        assert_eq!(
            occurrence.source.span().end_byte(),
            (start + name.len()) as u64
        );
    }
    let required = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Constant && entity.canonical_name == "required")
        .expect("written declared-instance requirement");
    let names: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.target
                == OccurrenceTarget::Resolved {
                    symbol: required.id,
                }
        })
        .collect();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].role, OccurrenceRole::Declaration);
    assert_eq!(
        names[0].source.span().start_byte(),
        source.find("required").unwrap() as u64
    );
    for name in ["C", "value", "body", "provided", "Module.prior"] {
        assert!(
            document.occurrences.iter().any(|occurrence| {
                occurrence.role == OccurrenceRole::Reference
                    && &source[usize::try_from(occurrence.source.span().start_byte()).unwrap()
                        ..usize::try_from(occurrence.source.span().end_byte()).unwrap()]
                        == name
                    && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            }),
            "missing unresolved source reference {name}"
        );
    }
    for name in [
        "input",
        "item",
        "field",
        "C",
        "value",
        "body",
        "Module.prior",
    ] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "invented declaration {name}"
        );
    }
    assert!(document.skipped_regions.iter().any(
        |gap| gap.detail == "coq-instance-typeclass-registration-and-field-binding-unavailable"
    ));
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "coq-module-section-stack-and-qualified-ownership-unavailable"));
}

#[test]
fn coq_original_unprefixed_instance_retains_its_exact_source_name() {
    let source = include_str!("../fixtures/coq-instances/subst_instance_list.v");
    assert_eq!(source.len(), 110);
    let result = output(source);
    let occurrence = named_definition(
        result.document(),
        "subst_instance_list",
        EntityKind::Constant,
    );
    assert_eq!(occurrence.source.span().start_byte(), 9);
    assert_eq!(occurrence.source.span().end_byte(), 28);
    assert!(result.document().skipped_regions.iter().any(|gap| gap.detail
        == "coq-term-and-module-binder-entities-and-lifetime-unavailable"));
}

#[test]
fn coq_instance_decoys_and_registrations_do_not_create_unwritten_declarations() {
    let source = "(* Instance comment_fake : C := value. *)\nDefinition text := \"Instance string_fake : C := value.\".\nFail Instance failed : C := value.\n#[export] Instance attributed : C := value.\nLocal Instance local_value : C := value.\n2: Instance selected : C := value.\nExisting Instance prior.\nExisting Class Carrier.\nInstance tail : C := @value.\nInstance methods : C := { field (item : nat) := @body }.\n";
    let result = output(source);
    named_definition(result.document(), "tail", EntityKind::Constant);
    named_definition(result.document(), "methods", EntityKind::Constant);
    for name in [
        "comment_fake",
        "string_fake",
        "failed",
        "attributed",
        "local_value",
        "selected",
        "prior",
        "Carrier",
        "field",
        "item",
    ] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "invented {name}"
        );
    }
    for name in ["value", "body", "prior", "Carrier"] {
        assert!(result.document().occurrences.iter().any(|occurrence| {
            occurrence.role == OccurrenceRole::Reference
                && &source[usize::try_from(occurrence.source.span().start_byte()).unwrap()
                    ..usize::try_from(occurrence.source.span().end_byte()).unwrap()]
                    == name
        }));
    }
    assert!(
        result.document().skipped_regions.iter().any(
            |gap| gap.detail == "coq-fail-attribute-or-selector-declaration-effect-unavailable"
        )
    );
    let malformed = output("Instance broken : C := { field := }.\n");
    let missing = malformed
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "syntax-error-recovery")
        .expect("the missing field value retains the native missing-node diagnostic");
    assert_eq!(missing.domain, FactDomain::Diagnostics);
    assert_eq!(missing.source.span().start_byte(), 33);
    assert_eq!(missing.source.span().end_byte(), 33);
    assert!(
        !malformed
            .document()
            .entities
            .iter()
            .any(|entity| { matches!(entity.canonical_name.as_str(), "broken" | "field") })
    );
    let recovered = output("Instance unclosed : C := ltac:(exact I.\n");
    let error = recovered
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "coq-parser-recovery-structure-unavailable")
        .expect("the unclosed Ltac term retains its native ERROR node");
    assert_eq!(error.source.span().start_byte(), 22);
    assert_eq!(error.source.span().end_byte(), 38);
    assert!(
        !recovered
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "unclosed")
    );
    for source in ["Instance _ : C := value.\n", "Declare Instance Type : C.\n"] {
        let result = output(source);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| { matches!(entity.canonical_name.as_str(), "_" | "Type") })
        );
        assert!(result.document().skipped_regions.iter().any(|gap| matches!(
            gap.detail.as_str(),
            "coq-declaration-name-unavailable" | "coq-parser-recovery-structure-unavailable"
        )));
    }
}

#[test]
fn coq_instance_identity_separates_declarations_and_ignores_body_edits() {
    let before = output("Instance provided : C := value.\n");
    let after = output("Instance provided : C := replacement.\n");
    let find = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "provided")
            .unwrap()
            .id
    };
    assert_eq!(find(&before), find(&after));
    for (result, expected) in [
        (&before, "Instance provided : C := value"),
        (&after, "Instance provided : C := replacement"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == find(result))
            .unwrap();
        let source = entity.evidence.source.as_ref().unwrap();
        assert_eq!(source.span().start_byte(), 0);
        assert_eq!(source.span().end_byte(), expected.len() as u64);
    }
    let repeated = output(
        "Instance same : C := value.\nModule Type Contract.\nDeclare Instance same : C.\nEnd Contract.\n",
    );
    let entities: Vec<_> = repeated
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "same")
        .collect();
    assert_eq!(entities.len(), 2);
    assert_ne!(entities[0].id, entities[1].id);
    for role in [OccurrenceRole::Definition, OccurrenceRole::Declaration] {
        assert_eq!(
            repeated
                .document()
                .occurrences
                .iter()
                .filter(|occurrence| occurrence.role == role)
                .count(),
            1 + usize::from(role == OccurrenceRole::Definition),
        );
    }
}

#[test]
fn coq_original_attributed_instances_remain_explicit_applicable_gaps() {
    for (source, name) in [
        (
            include_str!("../fixtures/coq-instances/dirpath_eqdec.v"),
            "dirpath_eqdec",
        ),
        (
            include_str!("../fixtures/coq-instances/reflect_prim_int.v"),
            "reflect_prim_int",
        ),
        (
            include_str!("../fixtures/coq-instances/ascii_eqdec.v"),
            "ascii_eqdec",
        ),
    ] {
        let result = output(source);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
        let gap = result
            .document()
            .skipped_regions
            .iter()
            .find(|gap| {
                gap.detail == "coq-fail-attribute-or-selector-declaration-effect-unavailable"
            })
            .unwrap();
        assert_eq!(gap.source.span().start_byte(), 0);
        assert_eq!(gap.source.span().end_byte(), source.len() as u64);
    }
}

#[test]
fn coq_projection_body_references_keep_exact_sources_and_unresolved_targets() {
    let text = "(* λ😀 *)\nDefinition argument := 0.\nDefinition result := value.(Module.field (argument := @supplied)).\nDefinition explicit := value.(@Module.field parameter).\nDefinition kept := 0.\n";
    for (encoding, bom) in [
        ("utf-8", false),
        ("utf-8", true),
        ("utf-16le", true),
        ("utf-16be", true),
    ] {
        let source = format!("{}{text}", if bom { "\u{feff}" } else { "" });
        let encode = |text: &str| -> Vec<u8> {
            if encoding == "utf-8" {
                text.as_bytes().to_vec()
            } else {
                text.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            }
        };
        let raw = encode(&source);
        let provider = Arc::new(provider());
        let fixture = Fixture::new(COQ, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(COQ),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, COQ),
            &request,
            &ExtensionSupport::default(),
        );
        validate(&result, &raw, encoding);
        named_definition(result.document(), "kept", EntityKind::Constant);
        let references: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .collect();
        let mut expected = Vec::new();
        for name in ["value", "Module.field", "supplied", "parameter"] {
            for (at, _) in source.match_indices(name) {
                let start = encode(&source[..at]).len();
                expected.push((
                    u64::try_from(start).unwrap(),
                    u64::try_from(start + encode(name).len()).unwrap(),
                    name,
                ));
            }
        }
        assert_eq!(
            references.len(),
            expected.len(),
            "{encoding}/{bom}: {references:?}"
        );
        for (start, end, name) in expected {
            let sites: Vec<_> = references
                .iter()
                .filter(|site| site.source.span().start_byte() == start)
                .collect();
            assert_eq!(sites.len(), 1, "{encoding}/{bom}: {name}");
            let site = sites[0];
            assert_eq!(site.source.span().end_byte(), end);
            assert_eq!(site.source.content_hash(), content_hash(&raw));
            assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
            assert_eq!(site.syntax_kind, "coq.written_reference.reference");
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert_eq!(
                result
                    .document()
                    .provenance
                    .iter()
                    .find(|p| p.id == site.provenance)
                    .unwrap()
                    .tier,
                AnalysisTier::TierD,
            );
        }
        assert!(result.document().skipped_regions.iter().any(|gap| gap.detail
            == "coq-local-term-binder-projection-and-generated-declaration-coverage-incomplete"));
    }
}

#[test]
fn coq_tactic_notation_is_not_a_global_declaration_or_resolved_effect() {
    let text = "(* λ😀 *)\nTactic Notation (at level 3) \"fabricated_function\" ident(H) := match type of H with | True => idtac | _ => idtac end.\nDefinition kept := 0.\n";
    for (encoding, bom) in [
        ("utf-8", false),
        ("utf-8", true),
        ("utf-16le", true),
        ("utf-16be", true),
    ] {
        let source = format!("{}{text}", if bom { "\u{feff}" } else { "" });
        let encode = |text: &str| -> Vec<u8> {
            if encoding == "utf-8" {
                text.as_bytes().to_vec()
            } else {
                text.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            }
        };
        let raw = encode(&source);
        let provider = Arc::new(provider());
        let fixture = Fixture::new(COQ, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(COQ),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, COQ),
            &request,
            &ExtensionSupport::default(),
        );
        validate(&result, &raw, encoding);
        let kept = named_definition(result.document(), "kept", EntityKind::Constant);
        let start = source.find("kept").unwrap();
        let end = start + "kept".len();
        let span = kept.source.span();
        assert_eq!(
            span.start_byte(),
            u64::try_from(encode(&source[..start]).len()).unwrap()
        );
        assert_eq!(
            span.end_byte(),
            u64::try_from(encode(&source[..end]).len()).unwrap()
        );
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.canonical_name != "fabricated_function"
                    && entity.canonical_name != "H")
        );
        assert!(
            result
                .document()
                .relations
                .iter()
                .all(|relation| relation.predicate != rootlight_ir::RelationPredicate::Calls)
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Reference)
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "coq-vernacular-construct-structure-unavailable")
        );
    }
}
