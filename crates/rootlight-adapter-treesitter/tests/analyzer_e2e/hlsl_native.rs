//! HLSL public facts retain exact written shader owners and generation-bound source.
//! Constant-buffer labels, annotations and call-shaped uses do not prove bindings.

use super::*;

const HLSL: LanguageCase = LanguageCase {
    name: "hlsl",
    path: "src/written.hlsl",
    frontend: "tree-sitter-hlsl-0.2.0",
    source: "float Scale(float Value) { return Value + 1.0; }\n",
    generated: false,
    body_before: "Value + 1.0",
    body_after: "Value  + 2.0",
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
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_eq!(file.generation, document.generation);
    assert_eq!(file.language, "hlsl");
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
                .diagnostics
                .iter()
                .filter_map(|diagnostic| diagnostic.source.as_ref()),
        )
        .chain(
            document
                .coverage_records
                .iter()
                .filter_map(|coverage| coverage.evidence.source.as_ref()),
        )
    {
        assert_eq!(reference.repository(), document.repository);
        assert_eq!(reference.generation(), document.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        let _ = text(source, reference);
    }
    for entity in &document.entities {
        assert_eq!(entity.language, "hlsl");
        assert_eq!(entity.generation, document.generation);
    }
    for coverage in &document.coverage_records {
        assert_eq!(
            coverage.discovered,
            coverage.indexed.checked_add(coverage.skipped).unwrap()
        );
    }
    for occurrence in &document.occurrences {
        assert_eq!(occurrence.generation, document.generation);
        assert_eq!(occurrence.file, file.id);
        let evidence = occurrence
            .evidence
            .source
            .as_ref()
            .expect("native occurrence retains direct source evidence");
        // Definition evidence includes its declaration context, not only its spelling.
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
            "hlsl.identifier.reference" | "hlsl.member.reference" | "hlsl.unproven_call.reference"
        ) {
            assert_eq!(occurrence.role, OccurrenceRole::Reference);
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
        assert!(!matches!(
            occurrence.role,
            OccurrenceRole::CallSite | OccurrenceRole::ImportUse
        ));
        assert!(!occurrence.syntax_kind.ends_with(".call_name"));
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
    let fixture = Fixture::new(HLSL, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, HLSL),
        &request(&fixture.snapshot, &fixture.source, HLSL, &budget),
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

fn assert_declaration(result: &AnalysisOutput, source: &str, name: &str, expected: &str) {
    assert_eq!(
        text(
            source,
            named(result, name).evidence.source.as_ref().unwrap()
        ),
        expected
    );
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
fn hlsl_functions_prototypes_and_parameters_keep_exact_semantic_headers() {
    let source = "float4 Shade(in float2 UV : TEXCOORD0, out float Amount) : SV_Target0 { Amount = UV.x; return float4(UV, 0.0, 1.0); }\nfloat Sample(float2 Coord : TEXCOORD1);\n";
    let result = output(source);
    assert!(result.document().diagnostics.is_empty());
    for name in ["Shade", "Sample"] {
        assert_definition(&result, source, name, EntityKind::Function);
    }
    for name in ["UV", "Amount", "Coord"] {
        assert_definition(&result, source, name, EntityKind::Parameter);
    }
    assert_owner(&result, "UV", "Shade");
    assert_owner(&result, "Amount", "Shade");
    assert_owner(&result, "Coord", "Sample");
    assert_declaration(&result, source, "Shade", source.lines().next().unwrap());
    assert_declaration(
        &result,
        source,
        "Sample",
        "float Sample(float2 Coord : TEXCOORD1);",
    );
    assert_declaration(&result, source, "UV", "in float2 UV : TEXCOORD0");
    assert_eq!(
        signatures(&result, source, "Shade"),
        ["float4 Shade(in float2 UV : TEXCOORD0, out float Amount) : SV_Target0"]
    );
    assert_eq!(
        signatures(&result, source, "Sample"),
        ["float Sample(float2 Coord : TEXCOORD1)"]
    );
    assert_eq!(
        signatures(&result, source, "UV"),
        ["in float2 UV : TEXCOORD0"]
    );
}

#[test]
fn hlsl_struct_fields_and_resource_variables_preserve_raw_headers_not_alias_names() {
    let source = "struct VertexInput { float3 Position : POSITION; float2 UV : TEXCOORD0; };\nTexture2D<float4> ColorTex : register(t0);\nSamplerState ColorSampler : register(s0);\n";
    let result = output(source);
    assert!(result.document().diagnostics.is_empty());
    assert_definition(&result, source, "VertexInput", EntityKind::Struct);
    for name in ["Position", "UV"] {
        assert_definition(&result, source, name, EntityKind::Field);
        assert_owner(&result, name, "VertexInput");
    }
    for name in ["ColorTex", "ColorSampler"] {
        assert_definition(&result, source, name, EntityKind::Variable);
    }
    assert_declaration(&result, source, "Position", "float3 Position : POSITION;");
    assert_declaration(
        &result,
        source,
        "ColorTex",
        "Texture2D<float4> ColorTex : register(t0);",
    );
    assert_eq!(
        signatures(&result, source, "VertexInput"),
        ["struct VertexInput"]
    );
    assert_eq!(
        signatures(&result, source, "Position"),
        ["float3 Position : POSITION"]
    );
    assert_eq!(signatures(&result, source, "UV"), ["float2 UV : TEXCOORD0"]);
    assert_eq!(
        signatures(&result, source, "ColorTex"),
        ["Texture2D<float4> ColorTex : register(t0)"]
    );
    assert_eq!(
        signatures(&result, source, "ColorSampler"),
        ["SamplerState ColorSampler : register(s0)"]
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Reference)
    );
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "hlsl-shader-annotation-and-profile-evaluation-unavailable",
        "register(t0)",
    );
}

#[test]
fn hlsl_buffer_labels_do_not_create_entities_scopes_types_or_bindings() {
    let source = "cbuffer Constants : register(b0) { float FirstValue; };\ncbuffer Constants : register(B2, space3) { float SecondValue; };\n";
    let result = output(source);
    assert!(result.document().diagnostics.is_empty());
    assert_eq!(result.document().entities.len(), 3);
    for name in ["FirstValue", "SecondValue"] {
        assert_definition(&result, source, name, EntityKind::Variable);
        assert!(!named(&result, name).qualified_name.contains("Constants"));
    }
    assert_declaration(&result, source, "FirstValue", "float FirstValue;");
    assert_declaration(&result, source, "SecondValue", "float SecondValue;");
    assert_eq!(
        signatures(&result, source, "FirstValue"),
        ["float FirstValue"]
    );
    assert_eq!(
        signatures(&result, source, "SecondValue"),
        ["float SecondValue"]
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Constants"
                || matches!(
                    entity.kind,
                    EntityKind::Struct | EntityKind::Class | EntityKind::Namespace
                ))
    );
    assert!(result.document().occurrences.iter().all(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            && matches!(
                text(source, &occurrence.source),
                "FirstValue" | "SecondValue"
            )
    }));
    for region in [
        "cbuffer Constants : register(b0)",
        "cbuffer Constants : register(B2, space3)",
    ] {
        assert_gap(
            &result,
            source,
            FactDomain::Relations,
            "hlsl-buffer-binding-and-layout-unavailable",
            region,
        );
    }
    assert_eq!(
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "hlsl-buffer-binding-and-layout-unavailable")
            .count(),
        2
    );
}

#[test]
fn hlsl_shared_fragments_conserve_distinct_names_without_borrowing_headers() {
    let source = "float First, Second[2];\nstruct Pair { float Left, Right; };\ncbuffer Constants { float Near, Far[2]; };\n";
    let result = output(source);
    assert!(result.document().diagnostics.is_empty());
    assert_definition(&result, source, "Pair", EntityKind::Struct);
    for name in ["First", "Second", "Near", "Far"] {
        assert_definition(&result, source, name, EntityKind::Variable);
    }
    for name in ["Left", "Right"] {
        assert_definition(&result, source, name, EntityKind::Field);
        assert_owner(&result, name, "Pair");
    }
    let fragments = ["First", "Second", "Left", "Right", "Near", "Far"];
    let ids = fragments
        .iter()
        .map(|name| named(&result, name).id)
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), fragments.len());
    assert_declaration(&result, source, "First", "First");
    assert_declaration(&result, source, "Second", "Second[2]");
    for name in fragments {
        assert!(
            signatures(&result, source, name).is_empty(),
            "shared header for {name}"
        );
    }
    assert_eq!(signatures(&result, source, "Pair"), ["struct Pair"]);
    for region in [
        "float First, Second[2]",
        "float Left, Right",
        "float Near, Far[2]",
    ] {
        assert_gap(
            &result,
            source,
            FactDomain::Extensions,
            "hlsl-shared-declaration-header-association-unavailable",
            region,
        );
    }
}

#[test]
fn hlsl_shader_contexts_and_initializer_uses_do_not_claim_selection_or_targets() {
    let source = "#define WEIGHT 2\n#if USE_COLOR\nfloat4 FirstColor;\n#else\nfloat4 SecondColor;\n#endif\n[numthreads(8, 8, 1)]\nvoid Dispatch(uint3 ID : SV_DispatchThreadID) { uint Index = ID.x; float4 Color = float4(Input, 0.0, 1.0); Result = Color.y; discard; }\n";
    let result = output(source);
    assert!(result.document().diagnostics.is_empty());
    for name in ["FirstColor", "SecondColor", "Index", "Color"] {
        assert_definition(&result, source, name, EntityKind::Variable);
    }
    assert_definition(&result, source, "Dispatch", EntityKind::Function);
    assert_definition(&result, source, "ID", EntityKind::Parameter);
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
    assert_eq!(
        references
            .iter()
            .map(|occurrence| text(source, &occurrence.source))
            .collect::<Vec<_>>(),
        ["ID", "x", "float4", "Input", "Result", "Color", "y"]
    );
    let labels = references
        .iter()
        .map(|occurrence| occurrence.syntax_kind.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        labels,
        [
            "hlsl.identifier.reference",
            "hlsl.member.reference",
            "hlsl.unproven_call.reference"
        ]
        .into_iter()
        .collect()
    );
    for occurrence in references {
        assert_gap(
            &result,
            source,
            FactDomain::Relations,
            "hlsl-binding-and-dispatch-target-unavailable",
            text(source, &occurrence.source),
        );
    }
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "hlsl-preprocessor-selection-unavailable",
        "#if USE_COLOR",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "hlsl-shader-annotation-and-profile-evaluation-unavailable",
        "numthreads",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "hlsl-body-binding-and-call-analysis-unavailable",
        "discard",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "hlsl-shader-binding-linking-and-preprocessing-unavailable",
        source,
    );
}

#[test]
fn hlsl_unsupported_parameters_and_recovery_keep_scoped_sibling_fences() {
    let source = "float Deferred(float Value = DefaultValue) { return Value; }\nfloat Visible(float Input) { return Input; }\n";
    let result = output(source);
    assert!(result.document().diagnostics.is_empty());
    assert_eq!(result.document().entities.len(), 3);
    assert_definition(&result, source, "Visible", EntityKind::Function);
    assert_definition(&result, source, "Input", EntityKind::Parameter);
    assert_gap(
        &result,
        source,
        FactDomain::Entities,
        "hlsl-parameter-owner-analysis-unavailable",
        "float Value = DefaultValue",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Entities,
        "hlsl-written-owner-analysis-unavailable",
        "float Deferred",
    );
    for source in [
        "float *Pointer;\nclass Foreign { float Member; };\nnamespace Foreign { float Value; }\n",
        "float First(), Second();",
        "float Broken(float Value) { return Value;",
    ] {
        let result = output(source);
        assert_eq!(result.document().entities.len(), 1, "{source}");
        assert!(result.document().occurrences.is_empty(), "{source}");
        assert_gap(
            &result,
            source,
            FactDomain::Entities,
            "hlsl-written-owner-analysis-unavailable",
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
fn hlsl_written_duplicates_body_edits_and_noop_repeats_conserve_identity() {
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
    let analyzer = analyzer(&provider, HLSL);
    let fixture = Fixture::new(HLSL, HLSL.source.as_bytes());
    let budget = limits();
    let run = |fixture: &Fixture| {
        analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, HLSL, &budget),
            &ExtensionSupport::default(),
        )
    };
    let before = run(&fixture);
    let repeated = run(&fixture);
    assert_sources(&before, HLSL.source);
    assert_sources(&repeated, HLSL.source);
    assert_eq!(before.document(), repeated.document());
    let edited = HLSL.source.replace(HLSL.body_before, HLSL.body_after);
    let changed = fixture.rewrite(edited.as_bytes());
    let after = run(&changed);
    assert_sources(&after, &edited);
    assert_eq!(symbol_ids(before.document()), symbol_ids(after.document()));
    assert_eq!(
        signatures(&before, HLSL.source, "Scale"),
        signatures(&after, &edited, "Scale")
    );
    assert_ne!(before.document().generation, after.document().generation);
    assert_ne!(
        before.document().files[0].content_hash,
        after.document().files[0].content_hash
    );
    let noop = changed.next_generation();
    let unchanged = run(&noop);
    assert_sources(&unchanged, &edited);
    assert_eq!(
        symbol_ids(after.document()),
        symbol_ids(unchanged.document())
    );
    assert_eq!(
        after.document().files[0].content_hash,
        unchanged.document().files[0].content_hash
    );
    assert_ne!(after.document().generation, unchanged.document().generation);
    assert_eq!(
        after.document().entities.len(),
        unchanged.document().entities.len()
    );
    assert_eq!(
        after.document().occurrences.len(),
        unchanged.document().occurrences.len()
    );
    assert_eq!(
        after.document().relations.len(),
        unchanged.document().relations.len()
    );
    assert_eq!(
        after.document().extensions.len(),
        unchanged.document().extensions.len()
    );
    assert_eq!(
        after.document().skipped_regions.len(),
        unchanged.document().skipped_regions.len()
    );
    assert_eq!(
        after.document().files.len(),
        unchanged.document().files.len()
    );
    assert_eq!(
        after.document().provenance.len(),
        unchanged.document().provenance.len()
    );
    assert_eq!(
        after.document().source_mappings.len(),
        unchanged.document().source_mappings.len()
    );
    assert_eq!(
        after.document().coverage_records.len(),
        unchanged.document().coverage_records.len()
    );
    assert_eq!(
        after.document().diagnostics.len(),
        unchanged.document().diagnostics.len()
    );
    let counts = |document: &rootlight_ir::NormalizedIrDocument| {
        let mut counts = document
            .coverage_records
            .iter()
            .map(|coverage| {
                (
                    coverage.domain,
                    coverage.discovered,
                    coverage.indexed,
                    coverage.skipped,
                )
            })
            .collect::<Vec<_>>();
        counts.sort_unstable();
        counts
    };
    assert_eq!(counts(after.document()), counts(unchanged.document()));
}

#[test]
fn hlsl_required_budget_conserves_complete_physical_identity_groups() {
    let source = "float Scale(float Value) { return Value; }\nfloat Other;\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, HLSL);
    let fixture = Fixture::new(HLSL, source.as_bytes());
    let full_limits = limits();
    let full_request = request(&fixture.snapshot, &fixture.source, HLSL, &full_limits);
    let full = analyzer
        .prepare_artifact(&full_request, &deadline())
        .unwrap();
    let required = full.required_syntax_fact_count(&deadline()).unwrap();
    assert!(required > 1 && full.syntax_fact_count() > required);
    let bounded_limits = limits_with_syntax_records(required);
    let bounded_request = request(&fixture.snapshot, &fixture.source, HLSL, &bounded_limits);
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
        matches!(analyzer.prepare_artifact(&request(&fixture.snapshot, &fixture.source, HLSL, &insufficient), &deadline()), Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit })) if observed == required && limit == required - 1)
    );
}
