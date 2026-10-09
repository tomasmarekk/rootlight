//! Public Assembly facts retain written owners and original source references.
//! Physical containment is not assembler, ISA, macro-expansion or operand binding.

use super::*;

const ASSEMBLY: LanguageCase = LanguageCase {
    name: "assembly",
    path: "src/definitions.asm",
    frontend: "tree-sitter-asm-0.24.0",
    source: "Run PROC NEAR\nmov rax, 1\nRun ENDP\n",
    generated: false,
    body_before: "rax, 1",
    body_after: "rax, 2",
};

const WRITTEN_OWNERS: &str = "Code SEGMENT\nRun PROC NEAR\nnested:\nmov rax, rbx\nRun ENDP\nExpand MACRO value\nmov rax, value\nENDM\nValue DB 1, 2\nSize EQU 4\nconst LIMIT 5\nlabel entry\nCode ENDS\n";

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(ASSEMBLY, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, ASSEMBLY),
        &request(&fixture.snapshot, &fixture.source, ASSEMBLY, &budget),
        &ExtensionSupport::default(),
    );
    assert_sources(&result, source);
    result
}

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
        assert!(
            source
                .get(
                    usize::try_from(reference.span().start_byte()).unwrap()
                        ..usize::try_from(reference.span().end_byte()).unwrap()
                )
                .is_some()
        );
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
    for occurrence in &document.occurrences {
        assert!(matches!(
            (occurrence.role, occurrence.syntax_kind.as_str()),
            (OccurrenceRole::Definition, _)
                | (OccurrenceRole::Documentation, "assembly.comment.comment")
                | (OccurrenceRole::StringEvidence, "assembly.string.string")
        ));
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(text(source, &occurrence.source).as_bytes())
        );
    }
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
    assert_eq!(
        definitions[0].syntactic_text_hash,
        content_hash(name.as_bytes())
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
                let reference = extension.evidence.source.as_ref().unwrap();
                lexical
                    .verify_source_bytes(text(source, reference).as_bytes())
                    .unwrap();
                assert_eq!(reference.generation(), result.document().generation);
                assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
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

fn assert_absent(result: &AnalysisOutput, names: &[&str]) {
    for name in names {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == *name),
            "unexpected entity {name}"
        );
    }
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
            let written = text(source, &gap.source);
            gap.domain == domain
                && gap.detail == detail
                && written.contains(region)
                && (domain != FactDomain::Occurrences || region == source || written != source)
        }),
        "missing {domain:?} gap {detail} at {region:?}"
    );
}

#[test]
fn assembly_eight_written_owners_keep_names_kinds_scopes_and_headers() {
    let result = output(WRITTEN_OWNERS);
    let module = named(&result, ASSEMBLY.path);
    assert_eq!(module.kind, EntityKind::Module);
    assert_eq!(module.flags, [EntityFlag::Synthetic]);
    assert_eq!(
        text(WRITTEN_OWNERS, module.evidence.source.as_ref().unwrap()),
        WRITTEN_OWNERS
    );
    for (name, kind) in [
        ("Code", EntityKind::Module),
        ("Run", EntityKind::Function),
        ("nested", EntityKind::StatementLabel),
        ("Expand", EntityKind::Function),
        ("Value", EntityKind::Variable),
        ("Size", EntityKind::Constant),
        ("LIMIT", EntityKind::Constant),
        ("entry", EntityKind::StatementLabel),
    ] {
        assert_definition(&result, WRITTEN_OWNERS, name, kind);
    }
    assert_eq!(result.document().entities.len(), 9);
    assert_eq!(result.document().occurrences.len(), 8);
    for (child, parent) in [
        ("Code", ASSEMBLY.path),
        ("Run", "Code"),
        ("nested", "Run"),
        ("Expand", "Code"),
        ("Value", "Code"),
        ("Size", "Code"),
        ("LIMIT", "Code"),
        ("entry", "Code"),
    ] {
        assert_owner(&result, child, parent);
    }
    assert_eq!(
        signatures(&result, WRITTEN_OWNERS, "Code"),
        ["Code SEGMENT"]
    );
    assert_eq!(
        signatures(&result, WRITTEN_OWNERS, "Run"),
        ["Run PROC NEAR"]
    );
    assert_eq!(
        signatures(&result, WRITTEN_OWNERS, "Expand"),
        ["Expand MACRO value"]
    );
    assert_absent(&result, &["value", "rax", "rbx"]);
}

#[test]
fn assembly_directives_operands_and_formals_retain_scoped_gaps_not_bindings() {
    let source = ".globl external\nExpand MACRO formal\nmov rax, formal\nENDM\n";
    let result = output(source);
    assert_definition(&result, source, "Expand", EntityKind::Function);
    assert_absent(&result, &["external", "formal", "mov", "rax"]);
    assert_eq!(result.document().entities.len(), 2);
    assert_gap(
        &result,
        source,
        FactDomain::Entities,
        "assembly-directive-owner-analysis-unavailable",
        ".globl external",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Entities,
        "assembly-macro-parameter-owner-analysis-unavailable",
        "Expand MACRO formal",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Occurrences,
        "assembly-source-use-analysis-unavailable",
        "mov rax, formal",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "assembly-expansion-and-binding-unavailable",
        source,
    );
}

#[test]
fn assembly_closers_keep_comments_case_and_empty_body_without_recovery_leaks() {
    for (source, name, kind) in [
        (
            "Mixed PROC\nMOV AX, BX\nmixed /* physical closer */ ENDP\n",
            "Mixed",
            EntityKind::Function,
        ),
        ("Empty MACRO\nENDM\n", "Empty", EntityKind::Function),
        (
            "Data SEGMENT\n; body trivia\nDATA ENDS\n",
            "Data",
            EntityKind::Module,
        ),
    ] {
        let result = output(source);
        assert_definition(&result, source, name, kind);
        assert_eq!(result.document().entities.len(), 2);
        if source.contains("physical closer") || source.contains("body trivia") {
            let comments = result
                .document()
                .occurrences
                .iter()
                .filter(|occurrence| occurrence.role == OccurrenceRole::Documentation)
                .collect::<Vec<_>>();
            assert_eq!(comments.len(), 1);
            assert_eq!(
                text(source, &comments[0].source),
                if name == "Mixed" {
                    "/* physical closer */"
                } else {
                    "; body trivia"
                }
            );
        }
    }
    for source in [
        "Run PROC\ninside:\nnop\nRunner ENDP\n",
        "Run PROC\ninside:\nnop\nRun ENDPextra\n",
        "Broken SEGMENT\nValue DB 1\nOther ENDS\n",
        "Empty MACRO\n; ENDM\n",
    ] {
        let result = output(source);
        assert_absent(&result, &["Run", "inside", "Broken", "Value", "Empty"]);
        assert_eq!(result.document().entities.len(), 1);
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .all(|occurrence| occurrence.role == OccurrenceRole::Documentation)
        );
        if source.contains("; ENDM") {
            assert_eq!(result.document().occurrences.len(), 1);
            assert_eq!(
                text(source, &result.document().occurrences[0].source),
                "; ENDM"
            );
        } else {
            assert!(result.document().occurrences.is_empty());
        }
        assert_gap(
            &result,
            source,
            FactDomain::Entities,
            "assembly-written-owner-analysis-unavailable",
            source.lines().next().unwrap(),
        );
    }
}

#[test]
fn assembly_numeric_labels_and_inline_recovery_do_not_invent_directional_uses() {
    let source = "1:\nnop\n.:\nnop\n.local:\nnop\nMixed:\nnop\n";
    let result = output(source);
    assert_definition(&result, source, ".local", EntityKind::StatementLabel);
    assert_definition(&result, source, "Mixed", EntityKind::StatementLabel);
    assert_absent(&result, &["1", ".", "nop"]);
    assert_eq!(result.document().entities.len(), 3);
    for region in ["1:", ".:"] {
        assert_gap(
            &result,
            source,
            FactDomain::Entities,
            "assembly-written-owner-analysis-unavailable",
            region,
        );
    }
    let inline_source = "entry: mov rax, rbx\n";
    let inline = output(inline_source);
    assert_definition(&inline, inline_source, "entry", EntityKind::StatementLabel);
    assert_absent(&inline, &["mov", "rax", "rbx"]);
    assert!(
        inline
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
    );
    assert_gap(
        &inline,
        inline_source,
        FactDomain::Occurrences,
        "assembly-source-use-analysis-unavailable",
        inline_source,
    );
}

#[test]
fn assembly_required_fact_budget_keeps_complete_owner_identity_groups() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, ASSEMBLY);
    let fixture = Fixture::new(ASSEMBLY, WRITTEN_OWNERS.as_bytes());
    let full_limits = limits();
    let full_request = request(&fixture.snapshot, &fixture.source, ASSEMBLY, &full_limits);
    let full_artifact = analyzer
        .prepare_artifact(&full_request, &deadline())
        .unwrap();
    let required = full_artifact
        .required_syntax_fact_count(&deadline())
        .unwrap();
    assert!(required > 1);
    assert!(full_artifact.syntax_fact_count() > required);
    let bounded_limits = limits_with_syntax_records(required);
    let bounded_request = request(
        &fixture.snapshot,
        &fixture.source,
        ASSEMBLY,
        &bounded_limits,
    );
    let bounded_artifact = analyzer
        .prepare_artifact(&bounded_request, &deadline())
        .unwrap();
    assert!(!bounded_artifact.syntax_partition_complete());
    assert_eq!(
        bounded_artifact
            .required_syntax_fact_count(&deadline())
            .unwrap(),
        required
    );
    let bounded = analyzer
        .analyze_prepared_artifact(
            &bounded_request,
            &bounded_artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_sources(&bounded, WRITTEN_OWNERS);
    assert_eq!(bounded.document().entities.len(), 9);
    for (name, kind) in [
        ("Code", EntityKind::Module),
        ("Run", EntityKind::Function),
        ("nested", EntityKind::StatementLabel),
        ("Expand", EntityKind::Function),
        ("Value", EntityKind::Variable),
        ("Size", EntityKind::Constant),
        ("LIMIT", EntityKind::Constant),
        ("entry", EntityKind::StatementLabel),
    ] {
        assert_definition(&bounded, WRITTEN_OWNERS, name, kind);
    }
    assert_eq!(
        signatures(&bounded, WRITTEN_OWNERS, "Code"),
        ["Code SEGMENT"]
    );
    assert_eq!(
        signatures(&bounded, WRITTEN_OWNERS, "Run"),
        ["Run PROC NEAR"]
    );
    assert_eq!(
        signatures(&bounded, WRITTEN_OWNERS, "Expand"),
        ["Expand MACRO value"]
    );
    assert_eq!(
        bounded.report().coverage().status(),
        CoverageStatus::Bounded
    );
    let insufficient = limits_with_syntax_records(required - 1);
    assert!(matches!(analyzer.prepare_artifact(
        &request(&fixture.snapshot, &fixture.source, ASSEMBLY, &insufficient), &deadline(),
    ), Err(AdapterError::Sink(SinkError::StreamLimit {
        resource: ResourceKind::RequiredSyntaxFacts, observed, limit,
    })) if observed == required && limit == required - 1));
}

#[test]
fn assembly_body_only_edits_keep_written_identities_and_refresh_source_refs() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, ASSEMBLY);
    let fixture = Fixture::new(ASSEMBLY, ASSEMBLY.source.as_bytes());
    let budget = limits();
    let before = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, ASSEMBLY, &budget),
        &ExtensionSupport::default(),
    );
    let edited = ASSEMBLY
        .source
        .replace(ASSEMBLY.body_before, ASSEMBLY.body_after);
    let changed = fixture.rewrite(edited.as_bytes());
    let after = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, ASSEMBLY, &budget),
        &ExtensionSupport::default(),
    );
    assert_sources(&before, ASSEMBLY.source);
    assert_sources(&after, &edited);
    assert_definition(&before, ASSEMBLY.source, "Run", EntityKind::Function);
    assert_definition(&after, &edited, "Run", EntityKind::Function);
    assert_eq!(symbol_ids(before.document()), symbol_ids(after.document()));
    assert_eq!(
        signatures(&before, ASSEMBLY.source, "Run"),
        signatures(&after, &edited, "Run")
    );
    assert_ne!(before.document().generation, after.document().generation);
    assert_ne!(
        before.document().files[0].content_hash,
        after.document().files[0].content_hash
    );
    assert_eq!(
        text(
            &edited,
            named(&after, "Run").evidence.source.as_ref().unwrap()
        ),
        edited.trim_end()
    );
}
