//! WGSL public facts retain written owners, raw names and generation-bound source.
//! Shader type, resource, attribute and invocation targets remain unproved.

use super::*;

const WGSL: LanguageCase = LanguageCase {
    name: "wgsl",
    path: "src/written.wgsl",
    frontend: "tree-sitter-wgsl-0.0.9",
    source: "struct Uniforms { scale: f32, }\nalias Δ = vec2<f32>;\nvar<private> counter: i32;\n@compute @workgroup_size(1)\nfn Main() { var local: i32 = 1; counter = local; }\n",
    generated: false,
    body_before: "counter = local",
    body_after: "counter  = local",
};

fn text<'source>(source: &'source str, reference: &SourceRef) -> &'source str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
}

fn assert_sources(result: &AnalysisOutput, source: &str) {
    let document = result.document();
    validate_ir_document(document, limits().ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.language, "wgsl");
    assert_eq!(file.generation, document.generation);
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    for reference in document
        .entities
        .iter()
        .filter_map(|entity| entity.evidence.source.as_ref())
        .chain(
            document
                .occurrences
                .iter()
                .map(|occurrence| &occurrence.source),
        )
        .chain(
            document
                .occurrences
                .iter()
                .filter_map(|occurrence| occurrence.evidence.source.as_ref()),
        )
        .chain(document.skipped_regions.iter().map(|gap| &gap.source))
        .chain(
            document
                .relations
                .iter()
                .filter_map(|relation| relation.evidence.source.as_ref()),
        )
        .chain(
            document
                .extensions
                .iter()
                .filter_map(|extension| extension.evidence.source.as_ref()),
        )
        .chain(
            document
                .coverage_records
                .iter()
                .filter_map(|coverage| coverage.evidence.source.as_ref()),
        )
        .chain(
            document
                .diagnostics
                .iter()
                .filter_map(|diagnostic| diagnostic.source.as_ref()),
        )
    {
        assert_eq!(reference.repository(), document.repository);
        assert_eq!(reference.generation(), document.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let _ = text(source, reference);
    }
    for entity in &document.entities {
        assert_eq!(entity.language, "wgsl");
        assert_eq!(entity.generation, document.generation);
    }
    for coverage in &document.coverage_records {
        assert_eq!(
            coverage.discovered,
            coverage.indexed.checked_add(coverage.skipped).unwrap()
        );
    }
    for occurrence in &document.occurrences {
        let evidence = occurrence.evidence.source.as_ref().unwrap();
        assert_eq!(occurrence.file, file.id);
        assert_eq!(occurrence.generation, document.generation);
        assert!(evidence.span().start_byte() <= occurrence.source.span().start_byte());
        assert!(occurrence.source.span().end_byte() <= evidence.span().end_byte());
        if !matches!(
            occurrence.role,
            OccurrenceRole::Definition | OccurrenceRole::Declaration
        ) {
            assert_eq!(evidence, &occurrence.source);
        }
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(text(source, &occurrence.source).as_bytes())
        );
        if matches!(
            occurrence.syntax_kind.as_str(),
            "wgsl.identifier.reference"
                | "wgsl.type.reference"
                | "wgsl.member.reference"
                | "wgsl.unproven_call.reference"
        ) {
            assert_eq!(occurrence.role, OccurrenceRole::Reference);
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
        assert!(!matches!(
            occurrence.role,
            OccurrenceRole::CallSite
                | OccurrenceRole::ImportUse
                | OccurrenceRole::Documentation
                | OccurrenceRole::StringEvidence
        ));
    }
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
}

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(WGSL, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, WGSL),
        &request(&fixture.snapshot, &fixture.source, WGSL, &budget),
        &ExtensionSupport::default(),
    );
    assert_sources(&result, source);
    result
}

fn named<'a>(result: &'a AnalysisOutput, name: &str) -> &'a rootlight_ir::EntityRecord {
    let entities = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name)
        .collect::<Vec<_>>();
    assert_eq!(
        entities.len(),
        1,
        "{name}: {:?}",
        result.document().entities
    );
    entities[0]
}

fn assert_definition(result: &AnalysisOutput, source: &str, name: &str, kind: EntityKind) {
    let entity = named(result, name);
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.display_name, name);
    let definitions = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1, "{name}");
    assert_eq!(text(source, &definitions[0].source), name);
}

fn signatures(result: &AnalysisOutput, source: &str, name: &str) -> Vec<String> {
    let id = named(result, name).id;
    result
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .filter_map(|extension| {
            let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                && lexical.subject() == rootlight_ir::FactRef::Entity(id))
            .then(|| {
                assert!(!lexical.is_truncated());
                lexical
                    .verify_source_bytes(
                        text(source, extension.evidence.source.as_ref().unwrap()).as_bytes(),
                    )
                    .unwrap();
                lexical.text().to_owned()
            })
        })
        .collect()
}

#[test]
fn wgsl_original_owners_have_exact_names_extents_headers_and_containment() {
    let result = output(WGSL.source);
    for (name, kind, owner) in [
        (
            "Uniforms",
            EntityKind::Struct,
            "struct Uniforms { scale: f32, }",
        ),
        ("scale", EntityKind::Field, "scale: f32"),
        ("Δ", EntityKind::TypeAlias, "alias Δ = vec2<f32>;"),
        (
            "counter",
            EntityKind::Variable,
            "var<private> counter: i32;",
        ),
        (
            "Main",
            EntityKind::Function,
            "@compute @workgroup_size(1)\nfn Main() { var local: i32 = 1; counter = local; }",
        ),
        ("local", EntityKind::Variable, "var local: i32 = 1;"),
    ] {
        assert_definition(&result, WGSL.source, name, kind);
        assert_eq!(
            text(
                WGSL.source,
                named(&result, name).evidence.source.as_ref().unwrap()
            ),
            owner
        );
    }
    assert_eq!(
        signatures(&result, WGSL.source, "Main"),
        ["@compute @workgroup_size(1)\nfn Main()"]
    );
    let parent = named(&result, "Uniforms");
    let child = named(&result, "scale");
    assert_eq!(
        child.container,
        Some(rootlight_ir::ContainerRef::Entity(parent.id))
    );
    assert!(result.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Contains
            && relation.subject == RelationEndpoint::Entity(parent.id)
            && relation.object == RelationEndpoint::Entity(child.id)
    }));
}

#[test]
fn wgsl_written_kinds_do_not_evaluate_constants_overrides_or_resource_annotations() {
    let source = "const Count: u32 = 4u;\n@id(3) override Capacity = Count;\nalias Color = vec4<f32>;\nvar<private> Input: Color;\nstruct Vertex { @location(0) position: Color, weight: f32, }\nfn Shade(value: Color) -> Color { let result = value; const local = Count; var mutable: Color = result; return Color(mutable); }\n";
    let result = output(source);
    for (name, kind) in [
        ("Count", EntityKind::Constant),
        ("Capacity", EntityKind::Variable),
        ("Color", EntityKind::TypeAlias),
        ("Input", EntityKind::Variable),
        ("Vertex", EntityKind::Struct),
        ("position", EntityKind::Field),
        ("weight", EntityKind::Field),
        ("Shade", EntityKind::Function),
        ("value", EntityKind::Parameter),
        ("result", EntityKind::Variable),
        ("local", EntityKind::Constant),
        ("mutable", EntityKind::Variable),
    ] {
        assert_definition(&result, source, name, kind);
    }
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Relations
            && gap.detail == "wgsl-attribute-and-pipeline-binding-unavailable"
    }));
}

#[test]
fn wgsl_type_member_and_call_spellings_remain_unresolved_with_scoped_gaps() {
    let source = "enable f16;\nstruct Written { field: f32, }\n@group(Group) @binding(Slot) var<storage, read> data: Written;\nfn Read(value: Written) -> f32 { return helper(value.field); }\n";
    let result = output(source);
    for (syntax, raw) in [
        ("wgsl.type.reference", "Written"),
        ("wgsl.member.reference", "field"),
        ("wgsl.unproven_call.reference", "helper"),
        ("wgsl.identifier.reference", "value"),
    ] {
        assert!(
            result.document().occurrences.iter().any(|occurrence| {
                occurrence.syntax_kind == syntax
                    && occurrence.role == OccurrenceRole::Reference
                    && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
                    && text(source, &occurrence.source) == raw
            }),
            "{syntax}: {raw}"
        );
    }
    for excluded in ["Group", "Slot", "storage", "read", "f16"] {
        assert!(!result.document().occurrences.iter().any(|occurrence| {
            occurrence.role == OccurrenceRole::Reference
                && text(source, &occurrence.source) == excluded
        }));
    }
    for detail in [
        "wgsl-binding-and-dispatch-target-unavailable",
        "wgsl-body-binding-and-call-analysis-unavailable",
        "wgsl-attribute-and-pipeline-binding-unavailable",
        "wgsl-directive-and-feature-validation-unavailable",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| { gap.domain == FactDomain::Relations && gap.detail == detail }),
            "{detail}"
        );
    }
}

#[test]
fn wgsl_hidden_comments_and_recovery_preserve_source_without_phantom_facts() {
    let source = "/* **/ alias Next = i32;\n/* outer /* inner */ close */ fn Later() {}\n";
    let result = output(source);
    assert_definition(&result, source, "Next", EntityKind::TypeAlias);
    assert_definition(&result, source, "Later", EntityKind::Function);
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Occurrences
            && gap.detail == "wgsl-hidden-comments-have-source-only-availability"
            && text(source, &gap.source) == source
    }));
    let broken = "alias Broken = array<vec2<f32>;\nalias After = i32;\n";
    let result = output(broken);
    assert_definition(&result, broken, "After", EntityKind::TypeAlias);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Broken")
    );
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Entities
            && gap.detail == "wgsl-written-owner-analysis-unavailable"
    }));
}

#[test]
fn wgsl_raw_unicode_and_repeated_local_owners_are_source_distinct() {
    let source = "struct Δ { e\u{301}: i32, }\nfn é(_value: Δ) { let e\u{301} = _value; }\n";
    let result = output(source);
    assert_definition(&result, source, "Δ", EntityKind::Struct);
    assert_definition(&result, source, "é", EntityKind::Function);
    assert_definition(&result, source, "_value", EntityKind::Parameter);
    let repeated = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "e\u{301}")
        .collect::<Vec<_>>();
    assert_eq!(repeated.len(), 2);
    assert_ne!(repeated[0].id, repeated[1].id);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "e")
    );
    assert!(
        repeated
            .iter()
            .all(|entity| entity.display_name == "e\u{301}")
    );
    let nested = "fn First(value: i32) { var same: i32 = value; { let same = value; } for (var i: i32 = 0; i < 1; i++) { let inner = i; } }\nfn Second(value: i32) { var same: i32 = value; }\n";
    let result = output(nested);
    let same = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "same")
        .collect::<Vec<_>>();
    assert_eq!(same.len(), 3);
    assert_eq!(
        same.iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    let definitions = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && text(nested, &occurrence.source) == "same"
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 3);
    assert_eq!(
        definitions
            .iter()
            .map(|occurrence| occurrence.source.span())
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
}

#[test]
fn wgsl_body_edits_keep_written_identity_without_reusing_generation_bound_source() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, WGSL);
    let fixture = Fixture::new(WGSL, WGSL.source.as_bytes());
    let budget = limits();
    let before = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, WGSL, &budget),
        &ExtensionSupport::default(),
    );
    let edited = WGSL.source.replace(WGSL.body_before, WGSL.body_after);
    let changed = fixture.rewrite(edited.as_bytes());
    let after = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, WGSL, &budget),
        &ExtensionSupport::default(),
    );
    assert_sources(&before, WGSL.source);
    assert_sources(&after, &edited);
    assert_eq!(symbol_ids(before.document()), symbol_ids(after.document()));
    assert_eq!(
        signatures(&before, WGSL.source, "Main"),
        signatures(&after, &edited, "Main")
    );
    assert_ne!(before.document().generation, after.document().generation);
    assert_ne!(
        before.document().files[0].content_hash,
        after.document().files[0].content_hash
    );
}
