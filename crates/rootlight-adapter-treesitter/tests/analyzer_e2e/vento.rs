//! Native Vento ownership does not imply JavaScript or host filter binding.

use super::*;

const VENTO: LanguageCase = LanguageCase {
    name: "vento",
    path: "native/template.vto",
    frontend: "tree-sitter-vento-0.21.0",
    source: include_str!("../../../../tests/native-grammars/fixtures/vento/badly_formatted.vto"),
    generated: false,
    body_before: "toUpperCase",
    body_after: "toLowerCase",
};

fn output(raw: &[u8]) -> AnalysisOutput {
    let native = Arc::new(provider());
    let analyzer = analyzer(&native, VENTO);
    let fixture = Fixture::new(VENTO, raw);
    analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, VENTO, &limits()),
        &ExtensionSupport::default(),
    )
}

#[test]
fn original_vento_filter_keeps_exact_written_name_and_unavailable_host_binding() {
    let raw = VENTO.source.as_bytes();
    assert_eq!(raw.len(), 52);
    let output = output(raw);
    let doc = output.document();
    assert_eq!(doc.files[0].language, "vento");
    assert_eq!(doc.files[0].content_hash, content_hash(raw));
    assert_eq!(
        doc.entities
            .iter()
            .filter(|e| e.kind == EntityKind::Module)
            .count(),
        1
    );
    let filters: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "vento.filter_name.reference")
        .collect();
    assert_eq!(filters.len(), 1, "{:?}", doc.skipped_regions);
    let site = filters[0];
    assert_eq!(site.role, OccurrenceRole::Reference);
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    let span = site.source.span();
    assert_eq!((span.start_byte(), span.end_byte()), (28, 39));
    assert_eq!(raw.get(28..39), Some(b"toUpperCase".as_slice()));
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert!(doc.skipped_regions.iter().any(|gap| {
        gap.detail == "vento-filter-host-binding-unavailable"
            && gap.source.span().start_byte() <= span.start_byte()
            && gap.source.span().end_byte() >= span.end_byte()
    }));
    assert!(
        doc.skipped_regions
            .iter()
            .any(|gap| gap.detail == "vento-javascript-expression-binding-unavailable")
    );
    assert!(
        !doc.occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::CallSite)
    );
}

#[test]
fn native_host_function_formal_and_set_names_keep_distinct_written_owners() {
    let raw = b"{{ function greet(name = \"x\") }}{{ set message = name }}{{ message |> toUpperCase }}{{ /function }}";
    let output = output(raw);
    let doc = output.document();
    let function = doc
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "greet")
        .expect("native function is present");
    for (kind, name) in [
        (EntityKind::Function, "greet"),
        (EntityKind::Parameter, "name"),
        (EntityKind::Variable, "message"),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|e| e.kind == kind && e.canonical_name == name)
            .collect();
        assert_eq!(
            entities.len(),
            1,
            "{name}: {:?}; {:?}",
            doc.entities,
            doc.skipped_regions
        );
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: entities[0].id,
                        }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let span = definitions[0].source.span();
        if kind != EntityKind::Function {
            assert_eq!(
                entities[0].container,
                Some(rootlight_ir::ContainerRef::Entity(function.id)),
                "{name}"
            );
            let ownership: Vec<_> = doc
                .relations
                .iter()
                .filter(|relation| {
                    relation.predicate == RelationPredicate::Contains
                        && relation.subject == RelationEndpoint::Entity(function.id)
                        && relation.object == RelationEndpoint::Entity(entities[0].id)
                })
                .collect();
            assert_eq!(ownership.len(), 1, "{name}");
            let evidence = ownership[0]
                .evidence
                .source
                .as_ref()
                .expect("native containment has source evidence");
            assert_eq!(evidence.content_hash(), content_hash(raw));
            assert!(
                evidence.span().start_byte() <= span.start_byte()
                    && evidence.span().end_byte() >= span.end_byte()
            );
        }
        assert_eq!(
            raw.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name.as_bytes())
        );
    }
    assert!(
        doc.occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .all(|site| site.syntax_kind == "vento.filter_name.reference"
                && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert!(
        !doc.occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::CallSite)
    );
    assert!(
        doc.skipped_regions
            .iter()
            .any(|gap| gap.detail == "vento-javascript-expression-binding-unavailable")
    );
}

#[test]
fn opaque_javascript_and_recovered_tags_cannot_donate_written_host_bindings() {
    for raw in [
        "{{ thing |> helpers.upper() }}",
        "{{ \"x\" | upper }}",
        "function fake(name) { set value = name; }",
        "{{ function broken(name) }}{{ set stolen = name }}",
    ] {
        let output = output(raw.as_bytes());
        let doc = output.document();
        assert!(
            !doc.entities.iter().any(|e| matches!(
                e.kind,
                EntityKind::Function | EntityKind::Parameter | EntityKind::Variable
            )),
            "{raw}: {:?}",
            doc.entities
        );
        assert!(
            !doc.occurrences.iter().any(|site| matches!(
                site.role,
                OccurrenceRole::Reference | OccurrenceRole::CallSite
            )),
            "{raw}"
        );
        if raw.starts_with("{{") {
            assert!(!doc.skipped_regions.is_empty(), "{raw}");
        }
    }
    let quoted = output(b"{{ \"x |> invented\" |> actual }}");
    let doc = quoted.document();
    let references: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference)
        .collect();
    assert_eq!(references.len(), 1);
    let span = references[0].source.span();
    assert_eq!(
        &b"{{ \"x |> invented\" |> actual }}"[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        b"actual"
    );
    let output = output(b"{{ function actual() }}body{{ /function }}{{ value |> actual }}");
    let doc = output.document();
    assert!(
        doc.entities
            .iter()
            .any(|e| e.kind == EntityKind::Function && e.canonical_name == "actual")
    );
    let filters: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "vento.filter_name.reference")
        .collect();
    assert_eq!(filters.len(), 1);
    assert!(matches!(
        filters[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
}
