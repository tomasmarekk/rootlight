//! GLSL facts describe exact written shader structure, not linked shader targets.

use super::*;

const GLSL: LanguageCase = LanguageCase {
    name: "glsl",
    path: "src/written.glsl",
    frontend: "tree-sitter-glsl-0.2.0",
    source: "float Scale(float Value) { return Value + 1.0; }\n",
    generated: false,
    body_before: "Value + 1.0",
    body_after: "Value  + 2.0",
};

fn text<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
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
    assert_eq!(
        document.files[0].content_hash,
        content_hash(source.as_bytes())
    );
    assert_eq!(
        document.files[0].byte_length,
        u64::try_from(source.len()).unwrap()
    );
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
    {
        assert_eq!(reference.generation(), document.generation);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        let _ = text(source, reference);
    }
    for occurrence in &document.occurrences {
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(text(source, &occurrence.source).as_bytes())
        );
        if matches!(
            occurrence.syntax_kind.as_str(),
            "glsl.identifier.reference" | "glsl.member.reference" | "glsl.unproven_call.reference"
        ) {
            assert_eq!(occurrence.role, OccurrenceRole::Reference);
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
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
    let fixture = Fixture::new(GLSL, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, GLSL),
        &request(&fixture.snapshot, &fixture.source, GLSL, &budget),
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

fn assert_owner(result: &AnalysisOutput, child: &str, parent: &str) {
    let child = named(result, child);
    let parent = named(result, parent);
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

fn assert_gap(
    result: &AnalysisOutput,
    source: &str,
    domain: FactDomain,
    detail: &str,
    region: &str,
) {
    assert!(
        result.document().skipped_regions.iter().any(|gap| {
            gap.domain == domain
                && gap.detail == detail
                && text(source, &gap.source).contains(region)
        }),
        "missing {domain:?} {detail} at {region:?}"
    );
}

#[test]
fn glsl_functions_and_prototypes_keep_parameters_and_exact_headers() {
    let source = "float Scale(in vec3 Position, out float Magnitude) { Magnitude = Position.x; return Magnitude; }\nfloat Sample(in vec2 UV);\n";
    let result = output(source);
    assert!(result.document().diagnostics.is_empty());
    for name in ["Scale", "Sample"] {
        assert_definition(&result, source, name, EntityKind::Function);
    }
    for name in ["Position", "Magnitude", "UV"] {
        assert_definition(&result, source, name, EntityKind::Parameter);
    }
    assert_owner(&result, "Position", "Scale");
    assert_owner(&result, "Magnitude", "Scale");
    assert_owner(&result, "UV", "Sample");
    assert_eq!(
        signatures(&result, source, "Scale"),
        ["float Scale(in vec3 Position, out float Magnitude)"]
    );
    assert_eq!(
        signatures(&result, source, "Sample"),
        ["float Sample(in vec2 UV)"]
    );
    assert_eq!(signatures(&result, source, "UV"), ["in vec2 UV"]);
}

#[test]
fn glsl_shared_fragments_and_interface_owners_preserve_written_structure() {
    let source = "float First, Second[2];\nstruct Pair { float Left, Right; };\nlayout(std140, binding = 0) uniform Camera { mat4 View; vec3 Eye; } camera;\nlayout(location = 1) in vec3 Position;\n";
    let result = output(source);
    assert!(result.document().diagnostics.is_empty());
    for name in ["First", "Second", "camera", "Position"] {
        assert_definition(&result, source, name, EntityKind::Variable);
    }
    for name in ["Pair", "Camera"] {
        assert_definition(&result, source, name, EntityKind::Struct);
    }
    for name in ["Left", "Right", "View", "Eye"] {
        assert_definition(&result, source, name, EntityKind::Field);
    }
    for child in ["Left", "Right"] {
        assert_owner(&result, child, "Pair");
    }
    for child in ["View", "Eye"] {
        assert_owner(&result, child, "Camera");
    }
    for name in ["First", "Second", "Left", "Right", "camera"] {
        assert!(
            signatures(&result, source, name).is_empty(),
            "shared header for {name}"
        );
    }
    assert_eq!(signatures(&result, source, "Pair"), ["struct Pair"]);
    assert_eq!(
        signatures(&result, source, "Camera"),
        ["layout(std140, binding = 0) uniform Camera"]
    );
    assert_gap(
        &result,
        source,
        FactDomain::Extensions,
        "glsl-shared-declaration-header-association-unavailable",
        "float First, Second[2]",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "glsl-shader-linking-and-interface-binding-unavailable",
        "uniform Camera",
    );
}

#[test]
fn glsl_shader_context_and_source_uses_do_not_claim_selection_or_targets() {
    let source = "#version 450\n#define WEIGHT 2\n#if USE_COLOR\nvec4 FirstColor;\n#else\nvec4 SecondColor;\n#endif\nprecision mediump float;\nvoid Shade(float) { discard; vec4 Color = texture(Image, UV); Result = Color.x; }\n";
    let result = output(source);
    assert!(result.document().diagnostics.is_empty());
    for name in ["FirstColor", "SecondColor", "Color"] {
        assert_definition(&result, source, name, EntityKind::Variable);
    }
    assert_definition(&result, source, "Shade", EntityKind::Function);
    let mut references = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect::<Vec<_>>();
    references.sort_unstable_by_key(|occurrence| {
        (
            occurrence.source.span().start_byte(),
            occurrence.source.span().end_byte(),
        )
    });
    let references = references
        .iter()
        .map(|occurrence| text(source, &occurrence.source))
        .collect::<Vec<_>>();
    assert_eq!(
        references,
        ["texture", "Image", "UV", "Result", "Color", "x"]
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::CallSite)
    );
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "glsl-preprocessor-selection-unavailable",
        "#if USE_COLOR",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "glsl-qualifier-and-profile-evaluation-unavailable",
        "precision mediump float",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Entities,
        "glsl-unnamed-parameter-owner-unavailable",
        "float",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "glsl-body-binding-and-call-analysis-unavailable",
        "discard",
    );
}

#[test]
fn glsl_inherited_c_and_recovery_do_not_fabricate_shader_definitions() {
    for source in [
        "float *Pointer;\nunion Foreign { float Member; };\nenum Choice { Item };\ntypedef float Alias;\nstatic float Hidden;\n",
        "float Values[static 2];",
        "void Run() Suffix() {}",
        "float Broken(float Value) { return Value;",
    ] {
        let result = output(source);
        assert_eq!(result.document().entities.len(), 1, "{source}");
        assert!(result.document().occurrences.is_empty(), "{source}");
        assert_gap(
            &result,
            source,
            FactDomain::Entities,
            "glsl-written-owner-analysis-unavailable",
            source.lines().next().unwrap(),
        );
    }
    let source = "/* float Phantom() { return Ghost; } λ */\n@;\nfloat Visible(float Value) { return Value; }\n";
    let result = output(source);
    assert_definition(&result, source, "Visible", EntityKind::Function);
    assert_definition(&result, source, "Value", EntityKind::Parameter);
    assert_eq!(result.document().entities.len(), 3);
    let comments = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Documentation)
        .collect::<Vec<_>>();
    assert_eq!(comments.len(), 1);
    assert_eq!(
        text(source, &comments[0].source),
        source.lines().next().unwrap()
    );
    assert!(
        result
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
    );
}

#[test]
fn glsl_duplicate_written_owners_and_body_edits_keep_physical_identity() {
    let source = "float Same(float Value);\nfloat Same(float Value) { return Value; }\n";
    let result = output(source);
    let functions = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "Same")
        .collect::<Vec<_>>();
    assert_eq!(functions.len(), 2);
    assert_ne!(functions[0].id, functions[1].id);
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, GLSL);
    let fixture = Fixture::new(GLSL, GLSL.source.as_bytes());
    let budget = limits();
    let before = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, GLSL, &budget),
        &ExtensionSupport::default(),
    );
    let edited = GLSL.source.replace(GLSL.body_before, GLSL.body_after);
    let changed = fixture.rewrite(edited.as_bytes());
    let after = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, GLSL, &budget),
        &ExtensionSupport::default(),
    );
    assert_sources(&before, GLSL.source);
    assert_sources(&after, &edited);
    assert_eq!(symbol_ids(before.document()), symbol_ids(after.document()));
    assert_eq!(
        signatures(&before, GLSL.source, "Scale"),
        signatures(&after, &edited, "Scale")
    );
    assert_ne!(before.document().generation, after.document().generation);
    assert_ne!(
        before.document().files[0].content_hash,
        after.document().files[0].content_hash
    );
}

#[test]
fn glsl_required_budget_conserves_complete_owner_identity_groups() {
    let source = "float Scale(float Value) { return Value; }\nfloat Other;\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, GLSL);
    let fixture = Fixture::new(GLSL, source.as_bytes());
    let full_limits = limits();
    let full_request = request(&fixture.snapshot, &fixture.source, GLSL, &full_limits);
    let full = analyzer
        .prepare_artifact(&full_request, &deadline())
        .unwrap();
    let required = full.required_syntax_fact_count(&deadline()).unwrap();
    assert!(required > 1 && full.syntax_fact_count() > required);
    let bounded_limits = limits_with_syntax_records(required);
    let bounded_request = request(&fixture.snapshot, &fixture.source, GLSL, &bounded_limits);
    let artifact = analyzer
        .prepare_artifact(&bounded_request, &deadline())
        .unwrap();
    assert!(!artifact.syntax_partition_complete());
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let result = analyzer
        .analyze_prepared_artifact(
            &bounded_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_sources(&result, source);
    assert_definition(&result, source, "Scale", EntityKind::Function);
    assert_definition(&result, source, "Value", EntityKind::Parameter);
    assert_definition(&result, source, "Other", EntityKind::Variable);
    assert_eq!(
        signatures(&result, source, "Scale"),
        ["float Scale(float Value)"]
    );
    assert_eq!(result.report().coverage().status(), CoverageStatus::Bounded);
    let insufficient = limits_with_syntax_records(required - 1);
    assert!(
        matches!(analyzer.prepare_artifact(&request(&fixture.snapshot, &fixture.source, GLSL, &insufficient), &deadline()), Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit })) if observed == required && limit == required - 1)
    );
}
