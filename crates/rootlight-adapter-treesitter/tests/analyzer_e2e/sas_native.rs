//! SAS written identities through the public analyzer and validated IR.
//! Physical source, owner and header proofs do not imply macro expansion,
//! compiler acceptance, runtime dataset identity or resolved call semantics.

use super::*;

const SAS: LanguageCase = LanguageCase {
    name: "sas",
    path: "src/definitions.sas",
    frontend: "tree-sitter-sas-0.4.2",
    source: "function Convert(Value); return Value+1; endsub;\n",
    generated: false,
    body_before: "Value+1",
    body_after: "Value+2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(SAS, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, SAS),
        &request(&fixture.snapshot, &fixture.source, SAS, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(
        result.document().files[0].content_hash,
        content_hash(source.as_bytes())
    );
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(source.len()).unwrap()
    );
    let modules = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Module)
        .collect::<Vec<_>>();
    assert_eq!(modules.len(), 1);
    assert_eq!(
        text(source, modules[0].evidence.source.as_ref().unwrap()),
        source
    );
    result
}

fn named<'a>(result: &'a AnalysisOutput, name: &str) -> &'a rootlight_ir::EntityRecord {
    let matches = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{name}: {:?}", result.document().entities);
    matches[0]
}

fn text<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
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
    let definition = definitions[0];
    assert_eq!(text(source, &definition.source), name);
    assert_eq!(
        definition.syntactic_text_hash,
        content_hash(name.as_bytes())
    );
    assert_eq!(definition.source.generation(), result.document().generation);
    assert_eq!(
        definition.source.content_hash(),
        content_hash(source.as_bytes())
    );
}

fn signatures(result: &AnalysisOutput, name: &str) -> Vec<String> {
    let entity = named(result, name);
    result
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .filter_map(|extension| {
            let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                && lexical.subject() == rootlight_ir::FactRef::Entity(entity.id))
            .then(|| {
                assert!(!lexical.is_truncated());
                lexical.text().to_owned()
            })
        })
        .collect()
}

fn assert_unqualified(result: &AnalysisOutput) {
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        result
            .document()
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|occurrence| occurrence.role != OccurrenceRole::CallSite)
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "sas-compiler-and-runtime-semantics-unavailable")
    );
}

#[test]
fn sas_fcmp_headers_formals_and_extents_preserve_original_case_and_crlf() {
    let source = "proc fcmp outlib=work.Library.Group;\r\n\
        FuNcTiOn Convert(Value, Label $) $;\r\nreturn Label;\r\nEnDsUb;\r\n\
        subroutine Adjust(Values[*], Count);\r\nCount=Count+1;\r\nendsub;\r\nquit;\r\n";
    let result = output(source);
    assert!(
        result.document().diagnostics.is_empty(),
        "{:?}",
        result.document().diagnostics
    );
    for (name, kind) in [
        ("Convert", EntityKind::Function),
        ("Adjust", EntityKind::Function),
        ("Value", EntityKind::Parameter),
        ("Label", EntityKind::Parameter),
        ("Values", EntityKind::Parameter),
        ("Count", EntityKind::Parameter),
    ] {
        assert_definition(&result, source, name, kind);
    }
    for (parameter, owner) in [
        ("Value", "Convert"),
        ("Label", "Convert"),
        ("Values", "Adjust"),
        ("Count", "Adjust"),
    ] {
        assert_eq!(
            named(&result, parameter).container,
            Some(rootlight_ir::ContainerRef::Entity(named(&result, owner).id)),
            "{parameter}"
        );
    }
    assert_eq!(
        signatures(&result, "Convert"),
        ["FuNcTiOn Convert(Value, Label $) $;"]
    );
    assert_eq!(
        signatures(&result, "Adjust"),
        ["subroutine Adjust(Values[*], Count);"]
    );
    assert_eq!(
        text(
            source,
            named(&result, "Convert").evidence.source.as_ref().unwrap()
        ),
        "FuNcTiOn Convert(Value, Label $) $;\r\nreturn Label;\r\nEnDsUb;"
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| ["fcmp", "Library", "Group"].contains(&entity.canonical_name.as_str()))
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "sas-fcmp-body-and-binding-semantics-unavailable")
    );
    assert_unqualified(&result);
}

#[test]
fn sas_character_return_lengths_preserve_header_and_formal_identity() {
    for length in [16, 2048] {
        let header = format!("function Label(Value $) $ {length};");
        let source = format!("{header}\r\nreturn Value;\r\nendsub;");
        let result = output(&source);
        assert!(result.document().diagnostics.is_empty());
        assert_definition(&result, &source, "Label", EntityKind::Function);
        assert_definition(&result, &source, "Value", EntityKind::Parameter);
        assert_eq!(
            named(&result, "Value").container,
            Some(rootlight_ir::ContainerRef::Entity(
                named(&result, "Label").id
            ))
        );
        assert_eq!(signatures(&result, "Label"), [header]);
        assert_eq!(
            text(
                &source,
                named(&result, "Label").evidence.source.as_ref().unwrap()
            ),
            source
        );
        assert!(
            result
                .document()
                .entities
                .iter()
                .filter(|entity| !matches!(entity.kind, EntityKind::File | EntityKind::Module))
                .all(|entity| matches!(entity.canonical_name.as_str(), "Label" | "Value"))
        );
        assert_unqualified(&result);
    }
}

#[test]
fn sas_macro_formals_keep_defaults_as_source_without_creating_default_definitions() {
    let source = "%macro Compose(First, Mode=DefaultName, Text='a,b', List=(One,Two), Ref=&External, Empty=) / secure;\n\
        %macro Nested(Inner); %put &Inner.; %mend nEsTeD;\n\
        %put &First.; %mend cOmPoSe;";
    let result = output(source);
    assert!(
        result.document().diagnostics.is_empty(),
        "{:?}",
        result.document().diagnostics
    );
    for name in ["Compose", "Nested"] {
        assert_definition(&result, source, name, EntityKind::Function);
    }
    for name in ["First", "Mode", "Text", "List", "Ref", "Empty"] {
        assert_definition(&result, source, name, EntityKind::Parameter);
        assert_eq!(
            named(&result, name).container,
            Some(rootlight_ir::ContainerRef::Entity(
                named(&result, "Compose").id
            ))
        );
    }
    assert_definition(&result, source, "Inner", EntityKind::Parameter);
    assert_eq!(
        named(&result, "Inner").container,
        Some(rootlight_ir::ContainerRef::Entity(
            named(&result, "Nested").id
        ))
    );
    assert_eq!(
        named(&result, "Nested").container,
        Some(rootlight_ir::ContainerRef::Entity(
            named(&result, "Compose").id
        ))
    );
    assert_eq!(
        signatures(&result, "Compose"),
        [
            "%macro Compose(First, Mode=DefaultName, Text='a,b', List=(One,Two), Ref=&External, Empty=) / secure;"
        ]
    );
    for name in ["DefaultName", "One", "Two", "External", "secure", "put"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "{name}"
        );
    }
    assert!(result.document().occurrences.iter().any(|occurrence| {
        occurrence.syntax_kind == "sas.macro_variable.reference"
            && text(source, &occurrence.source) == "&First."
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    }));
    assert_unqualified(&result);
}

#[test]
fn sas_data_single_and_multiple_outputs_have_noncolliding_exact_source_identities() {
    let source = "data Work.Output; Value=1; run;\n\
        data Work.Left Work.Right; Value=2; run;\n\
        data _null_; Value=3; run;\n\
        data Work.&Chosen.; Value=4; run;";
    let result = output(source);
    for name in ["Work.Output", "Work.Left", "Work.Right"] {
        assert_definition(&result, source, name, EntityKind::DatabaseObject);
    }
    assert_eq!(signatures(&result, "Work.Output"), ["data Work.Output;"]);
    assert_eq!(
        text(
            source,
            named(&result, "Work.Output")
                .evidence
                .source
                .as_ref()
                .unwrap()
        ),
        "data Work.Output; Value=1; run;"
    );
    for name in ["Work.Left", "Work.Right"] {
        assert!(signatures(&result, name).is_empty());
        assert_eq!(
            text(
                source,
                named(&result, name).evidence.source.as_ref().unwrap()
            ),
            name
        );
    }
    assert_ne!(
        named(&result, "Work.Left").id,
        named(&result, "Work.Right").id
    );
    assert_eq!(
        named(&result, "Work.Left").container,
        named(&result, "Work.Right").container
    );
    for name in [
        "Work",
        "Output",
        "Left",
        "Right",
        "_null_",
        "Chosen",
        "Work.&Chosen.",
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
    for gap in [
        "sas-shared-data-header-and-runtime-semantics-unavailable",
        "sas-dynamic-or-special-dataset-identity-unavailable",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|region| region.detail == gap),
            "{gap}"
        );
    }
    assert_unqualified(&result);
}

#[test]
fn sas_comment_bridged_dynamic_outputs_keep_source_and_unresolved_macro_leaves() {
    for first in ["First", "&First"] {
        let written = format!("{first}/* gap */&Next");
        let source = format!("data {written}; run;");
        let result = output(&source);
        assert!(result.document().diagnostics.is_empty());
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| matches!(entity.kind, EntityKind::File | EntityKind::Module)),
            "a comment-bridged dynamic name must not become a static entity"
        );
        let gaps = result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "sas-dynamic-or-special-dataset-identity-unavailable")
            .collect::<Vec<_>>();
        assert_eq!(gaps.len(), 3);
        for domain in [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ] {
            let matches = gaps
                .iter()
                .filter(|gap| gap.domain == domain)
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1);
            let gap = matches[0];
            assert_eq!(text(&source, &gap.source), written);
            assert_eq!(gap.source.generation(), result.document().generation);
            assert_eq!(gap.source.content_hash(), content_hash(source.as_bytes()));
        }
        let references = result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.syntax_kind == "sas.macro_variable.reference")
            .collect::<Vec<_>>();
        let expected = if first.starts_with('&') {
            vec!["&First", "&Next"]
        } else {
            vec!["&Next"]
        };
        assert_eq!(references.len(), expected.len());
        for expected in expected {
            let matches = references
                .iter()
                .filter(|occurrence| text(&source, &occurrence.source) == expected)
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1);
            let occurrence = matches[0];
            let start = u64::try_from(source.find(expected).unwrap()).unwrap();
            assert_eq!(occurrence.source.span().start_byte(), start);
            assert_eq!(
                occurrence.source.span().end_byte(),
                start + u64::try_from(expected.len()).unwrap()
            );
            assert_eq!(occurrence.source.generation(), result.document().generation);
            assert_eq!(
                occurrence.source.content_hash(),
                content_hash(source.as_bytes())
            );
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(expected.as_bytes())
            );
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
        assert_unqualified(&result);
    }
}

#[test]
fn sas_proc_dataset_and_macro_references_do_not_become_catalog_or_call_bindings() {
    let source = "%macro Render(Input); %put &Input.; %mend Render;\n\
        %Render(Value); %macroFoo(Value); %letput Value;\n\
        data Work.Output; set Work.Input; output Work.Other; run;\n\
        proc print data=Work.Output; run; %include 'missing-source.sas';";
    let result = output(source);
    for name in [
        "print",
        "put",
        "macroFoo",
        "letput",
        "Work.Input",
        "Work.Other",
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
    for syntax in [
        "sas.macro_call.reference",
        "sas.dataset.reference",
        "sas.macro_variable.reference",
    ] {
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.syntax_kind == syntax),
            "{syntax}"
        );
    }
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role != OccurrenceRole::Definition)
            .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert_unqualified(&result);
}

#[test]
fn sas_malformed_owners_cannot_leak_formals_or_borrow_good_sibling_terminators() {
    for source in [
        "function Broken(Leaked); Leaked=1;\nfunction Good(Kept); return Kept; endsub;",
        "subroutine Broken(Leaked); Leaked=1;\nsubroutine Good(Kept); Kept=Kept+1; endsub;",
        "%macro Broken(Leaked); %put &Leaked.; %mend Other;\n%macro Good(Kept); %put &Kept.; %mend Good;",
        "function Broken(Leaked,,Other); return Leaked; endsub;\nfunction Good(Kept); return Kept; endsub;",
    ] {
        let result = output(source);
        for name in ["Broken", "Leaked", "Other"] {
            assert!(
                !result
                    .document()
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == name),
                "{name}: {source}"
            );
        }
        assert_definition(&result, source, "Good", EntityKind::Function);
        assert_definition(&result, source, "Kept", EntityKind::Parameter);
        assert_eq!(
            named(&result, "Kept").container,
            Some(rootlight_ir::ContainerRef::Entity(
                named(&result, "Good").id
            ))
        );
        assert_unqualified(&result);
    }
    let source = "data Work.Broken; Value=1;\ndata Work.Good; Value=2; run;";
    let result = output(source);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Work.Broken")
    );
    assert_definition(&result, source, "Work.Good", EntityKind::DatabaseObject);
}

#[test]
fn sas_body_only_edits_keep_written_function_and_formal_ids_with_new_source_generation() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, SAS);
    let fixture = Fixture::new(SAS, SAS.source.as_bytes());
    let budget = limits();
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, SAS, &budget),
        &ExtensionSupport::default(),
    );
    let changed = fixture.rewrite(
        SAS.source
            .replace(SAS.body_before, SAS.body_after)
            .as_bytes(),
    );
    let next = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, SAS, &budget),
        &ExtensionSupport::default(),
    );
    assert_eq!(
        first
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>(),
        next.document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    );
    assert_ne!(fixture.source.generation(), changed.source.generation());
    for entity in &next.document().entities {
        let source = entity.evidence.source.as_ref().unwrap();
        assert_eq!(source.generation(), changed.source.generation());
        assert_eq!(source.content_hash(), changed.source.content_hash());
    }
    assert_unqualified(&next);
}

#[test]
fn sas_quoted_and_commented_header_text_is_not_a_definition() {
    let source = "function Outer(Value); Text='function Fake(X); endsub;';\n\
        /* subroutine Hidden(Y); endsub; */\n\
        * function Commented(Z);\nreturn Value; endsub;";
    let result = output(source);
    assert_definition(&result, source, "Outer", EntityKind::Function);
    assert_definition(&result, source, "Value", EntityKind::Parameter);
    for name in ["Fake", "Hidden", "Commented", "X", "Y", "Z", "Text"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "{name}"
        );
    }
    assert_unqualified(&result);
}

#[test]
fn sas_analysis_checks_cancellation_before_native_work() {
    let fixture = Fixture::new(SAS, SAS.source.as_bytes());
    let budget = limits();
    let (analyzer, parser) = counting_analyzer(SAS, ParseTestBehavior::Native);
    let cancelled = deadline();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyzer.prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, SAS, &budget),
            &cancelled
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert_eq!(parser.invocations.load(Ordering::Relaxed), 0);
}
