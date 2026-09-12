//! Groovy declarations through the public analyzer and generation-bound source evidence.
//! Parsing alone cannot establish discoverable entities or exact definition ownership.

use super::*;

pub(super) const GROOVY: LanguageCase = LanguageCase {
    name: "groovy",
    path: "src/catalog.groovy",
    frontend: "dekobon-tree-sitter-groovy-0.2.0",
    source: "class Meter { String title; private int value; String read() { 'x' } }\nString render() { 'y' }\n",
    generated: false,
    body_before: "'x'",
    body_after: "'updated'",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(GROOVY, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, GROOVY),
        &request(&fixture.snapshot, &fixture.source, GROOVY, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn groovy_declarations_keep_exact_definition_sources() {
    let result = output(GROOVY.source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind) in [
        ("Meter", EntityKind::Class),
        ("title", EntityKind::Property),
        ("value", EntityKind::Field),
        ("read", EntityKind::Method),
        ("render", EntityKind::Function),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(entities.len(), 1, "{name}/{kind:?}: {:?}", doc.entities);
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == (OccurrenceTarget::Resolved {
                            symbol: entities[0].id,
                        })
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let span = definitions[0].source.span();
        assert_eq!(
            GROOVY.source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name)
        );
    }
}

#[test]
fn groovy_multiple_bindings_and_parameters_have_distinct_owners() {
    let source = "trait Named { String label() { 'name' } }\nclass Meter { private int first, second = 2; Meter(int seed) { first = seed }\nint read(int delta) { int left = 1, right = 2; return left + right + delta } }\nenum Mode { OPEN, CLOSED }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind) in [
        ("Named", EntityKind::Trait),
        ("label", EntityKind::Method),
        ("Meter", EntityKind::Class),
        ("Meter", EntityKind::Constructor),
        ("first", EntityKind::Field),
        ("second", EntityKind::Field),
        ("seed", EntityKind::Parameter),
        ("read", EntityKind::Method),
        ("delta", EntityKind::Parameter),
        ("left", EntityKind::Variable),
        ("right", EntityKind::Variable),
        ("Mode", EntityKind::Enum),
        ("OPEN", EntityKind::Constant),
        ("CLOSED", EntityKind::Constant),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(
            entities.len(),
            1,
            "{name}/{kind:?}: {:?}; {:?}",
            doc.entities,
            doc.skipped_regions
        );
        assert!(entities[0].container.is_some(), "{name}");
    }
}

#[test]
fn groovy_body_edits_preserve_declaration_identity() {
    let before = output(GROOVY.source);
    let after = output(&GROOVY.source.replace(GROOVY.body_before, GROOVY.body_after));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.canonical_name.clone(), entity.id))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&before), identities(&after));
}

#[test]
fn groovy_properties_and_explicit_fields_keep_exact_authored_identity() {
    let source = "class Ledger { String title; private int secret; protected int guarded; public int exposed; static int count; final int version = 1; String first, second; @Deprecated String legacy }\ntrait Named { String label; private int slot }\ninterface Limits { int MAX = 10 }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind, owner) in [
        ("title", EntityKind::Property, "Ledger"),
        ("secret", EntityKind::Field, "Ledger"),
        ("guarded", EntityKind::Field, "Ledger"),
        ("exposed", EntityKind::Field, "Ledger"),
        ("count", EntityKind::Property, "Ledger"),
        ("version", EntityKind::Property, "Ledger"),
        ("first", EntityKind::Property, "Ledger"),
        ("second", EntityKind::Property, "Ledger"),
        ("legacy", EntityKind::Property, "Ledger"),
        ("label", EntityKind::Property, "Named"),
        ("slot", EntityKind::Field, "Named"),
        ("MAX", EntityKind::Field, "Limits"),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect();
        assert_eq!(entities.len(), 1, "{name}: {entities:?}");
        let entity = entities[0];
        assert_eq!(entity.kind, kind, "{name}");
        let container = doc
            .entities
            .iter()
            .find(|candidate| {
                Some(rootlight_ir::ContainerRef::Entity(candidate.id)) == entity.container
            })
            .unwrap();
        assert_eq!(container.canonical_name, owner, "{name}");
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let span = definitions[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name)
        );
    }
    assert!(
        !doc.entities
            .iter()
            .any(|entity| matches!(entity.kind, EntityKind::Method | EntityKind::Function)),
        "implicit accessors have no authored declarations"
    );
}

#[test]
fn groovy_dynamic_receiver_is_not_resolved_by_matching_method_name() {
    let source = "class Meter { int read() { 1 } }\ndef run(target) { target.read() }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let calls: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    assert!(!matches!(
        calls[0].target,
        OccurrenceTarget::Resolved { .. }
    ));
    assert!(
        !doc.relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(doc.skipped_regions.iter().any(|gap| {
        gap.detail == "groovy-import-inheritance-and-runtime-dispatch-resolution-unavailable"
    }));
}

#[test]
fn groovy_quoted_method_names_keep_decoded_identity_and_exact_written_definitions() {
    for (written, name) in [
        ("'with space'", "with space"),
        ("\"double space\"", "double space"),
        ("'''triple space'''", "triple space"),
        ("\"\"\"triple double\"\"\"", "triple double"),
        ("/slash name/", "slash name"),
        ("$/dollar name/$", "dollar name"),
        (r"'uni\u0063ode'", "unicode"),
        (r"'quote\'name'", "quote'name"),
        (r"'surrogate\uD83D\uDE80'", "surrogate🚀"),
        (r"'line\nname'", r#""line\u000aname""#),
        (r"'lone\uD800'", r#""lone\ud800""#),
        ("''", "\"\""),
    ] {
        let source = format!("class Sample {{ def {written}() {{ 1 }} }}\n");
        let result = output(&source);
        let doc = result.document();
        assert!(
            doc.diagnostics.is_empty(),
            "{written}: {:?}",
            doc.diagnostics
        );
        let methods: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Method)
            .collect();
        assert_eq!(
            methods.len(),
            1,
            "{written}: {:?}; {:?}",
            doc.entities,
            doc.skipped_regions
        );
        assert_eq!(methods[0].canonical_name, name, "{written}");
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == OccurrenceTarget::Resolved {
                            symbol: methods[0].id,
                        }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{written}");
        let span = definitions[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
        assert_eq!(
            definitions[0].syntactic_text_hash,
            content_hash(written.as_bytes())
        );
        assert!(
            !doc.skipped_regions
                .iter()
                .any(|gap| gap.detail == "groovy-quoted-name-unavailable")
        );
    }
}

#[test]
fn groovy_unavailable_method_names_report_exact_gaps_without_inventing_local_owners() {
    for written in [r#""${name}""#, r#""$name""#, r"'bad\q'"] {
        let source = format!(
            "class Sample {{ def {written}(parameter) {{ def localValue = 1 }}\ndef valid() {{ 2 }} }}\n"
        );
        let result = output(&source);
        let doc = result.document();
        assert!(
            doc.diagnostics.is_empty(),
            "{written}: {:?}",
            doc.diagnostics
        );
        let methods: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Method)
            .map(|entity| entity.canonical_name.as_str())
            .collect();
        assert_eq!(methods, ["valid"], "{written}");
        assert!(
            !doc.entities
                .iter()
                .any(|entity| matches!(entity.canonical_name.as_str(), "parameter" | "localValue"))
        );
        let gaps: Vec<_> = doc
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "groovy-quoted-name-unavailable")
            .collect();
        assert_eq!(gaps.len(), 1, "{written}: {:?}", doc.skipped_regions);
        let span = gaps[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
    }
}

#[test]
fn groovy_closures_own_explicit_parameters_and_locals_without_written_names() {
    for (source, callable, header, bindings) in [
        (
            "def transform = { String value -> def local = value; local }\n",
            "{ String value -> def local = value; local }",
            "{ String value ->",
            vec!["value", "local"],
        ),
        (
            "class Store { def transform = { value -> value } }\n",
            "{ value -> value }",
            "{ value ->",
            vec!["value"],
        ),
        (
            "def run() { consume { value -> value } }\n",
            "{ value -> value }",
            "{ value ->",
            vec!["value"],
        ),
        (
            "def message = \"${writer -> writer << 'ready'}\"\n",
            "writer -> writer << 'ready'",
            "writer ->",
            vec!["writer"],
        ),
        ("def callback = { -> 1 }\n", "{ -> 1 }", "{ ->", vec![]),
        ("def callback = { it }\n", "{ it }", "{", vec![]),
    ] {
        let result = output(source);
        let doc = result.document();
        assert!(
            doc.diagnostics.is_empty(),
            "{source}: {:?}",
            doc.diagnostics
        );
        let functions: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "<anonymous>")
            .collect();
        assert_eq!(functions.len(), 1, "{source}: {:?}", doc.entities);
        let function = functions[0];
        assert_eq!(function.kind, EntityKind::Function);
        assert!(
            function
                .flags
                .contains(&rootlight_ir::EntityFlag::Synthetic)
        );
        let signatures: Vec<_> = doc
            .extensions
            .iter()
            .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|extension| {
                let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
                if lexical.kind() != rootlight_ir::LexicalEvidenceKind::Signature
                    || lexical.subject() != rootlight_ir::FactRef::Entity(function.id)
                {
                    return None;
                }
                let span = extension.evidence.source.as_ref().unwrap().span();
                assert_eq!(
                    source.get(
                        usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap()
                    ),
                    Some(lexical.text())
                );
                Some(lexical.text().to_owned())
            })
            .collect();
        assert_eq!(signatures, [header]);
        let span = function.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(callable)
        );
        assert!(!doc.occurrences.iter().any(|occurrence| occurrence.role
            == OccurrenceRole::Definition
            && occurrence.target
                == OccurrenceTarget::Resolved {
                    symbol: function.id
                }));
        for name in bindings {
            let binding = doc
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            assert_eq!(
                binding.container,
                Some(rootlight_ir::ContainerRef::Entity(function.id)),
                "{name}"
            );
            assert!(
                doc.relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::Contains
                        && relation.subject == RelationEndpoint::Entity(function.id)
                        && relation.object == RelationEndpoint::Entity(binding.id))
            );
        }
        assert!(
            !doc.entities
                .iter()
                .any(|entity| entity.canonical_name == "it"),
            "implicit parameters have no authored definition"
        );
    }
}

#[test]
fn groovy_sibling_and_nested_closures_keep_distinct_body_independent_identities() {
    let source =
        "def run() { consume({ value -> value }, { value -> { nested -> nested + value } }) }\n";
    let before = output(source);
    let doc = before.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let closures: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "<anonymous>")
        .collect();
    assert_eq!(closures.len(), 3, "{:?}", doc.entities);
    assert_eq!(
        closures
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    for (name, owner_source) in [
        ("value", "{ value -> value }"),
        ("value", "{ value -> { nested -> nested + value } }"),
        ("nested", "{ nested -> nested + value }"),
    ] {
        let owner = closures
            .iter()
            .find(|entity| {
                let span = entity.evidence.source.as_ref().unwrap().span();
                source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                ) == Some(owner_source)
            })
            .unwrap();
        assert_eq!(
            doc.entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(owner.id)))
                .count(),
            1
        );
    }
    let after = output(&source.replace("nested + value", "nested + value + 12345"));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.id, entity.canonical_name.clone(), entity.container))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(identities(&before), identities(&after));
}

#[test]
fn groovy_disjoint_block_variables_keep_distinct_source_identities() {
    let source = "def run() { if (true) { def local = 1 }; if (false) { def local = 2 } }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let locals: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "local")
        .collect();
    assert_eq!(
        locals.len(),
        2,
        "{:?}; {:?}",
        doc.entities,
        doc.skipped_regions
    );
    assert_ne!(locals[0].id, locals[1].id);
    let after = output(&source.replace("= 1", "= 12345"));
    let ids = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(&result), ids(&after));
}
