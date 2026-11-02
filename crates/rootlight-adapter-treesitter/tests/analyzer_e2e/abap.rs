//! Native ABAP interfaces retain authored evidence and explicit untyped clauses.

use super::*;

const ABAP: LanguageCase = LanguageCase {
    name: "abap",
    path: "native/interface.abap",
    frontend: "tree-sitter-abap-0.1.0",
    source: include_str!(
        "../../../../tests/native-grammars/fixtures/abap/zif_abapgit_gui_renderable.intf.abap"
    ),
    generated: false,
    body_before: "render",
    body_after: "paint",
};

fn output(raw: &[u8]) -> AnalysisOutput {
    let native = Arc::new(provider());
    let analyzer = analyzer(&native, ABAP);
    let fixture = Fixture::new(ABAP, raw);
    analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, ABAP, &limits()),
        &ExtensionSupport::default(),
    )
}

#[test]
fn original_abap_interface_has_two_written_definitions_and_contains() {
    let raw = ABAP.source.as_bytes();
    assert_eq!(raw.len(), 186);
    let output = output(raw);
    let doc = output.document();
    assert_eq!(doc.files[0].language, "abap");
    assert_eq!(doc.files[0].content_hash, content_hash(raw));
    let owner = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Interface)
        .expect("native interface is present");
    let method = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Method)
        .expect("native method is present");
    assert_eq!(owner.canonical_name, "zif_abapgit_gui_renderable");
    assert_eq!(method.canonical_name, "render");
    for (entity, expected) in [(owner, (10, 36)), (method, (59, 65))] {
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{:?}", doc.skipped_regions);
        let source = &definitions[0].source;
        assert_eq!(
            (source.span().start_byte(), source.span().end_byte()),
            expected
        );
        assert_eq!(source.content_hash(), content_hash(raw));
    }
    assert_eq!(
        method.container,
        Some(rootlight_ir::ContainerRef::Entity(owner.id))
    );
    let contains: Vec<_> = doc
        .relations
        .iter()
        .filter(|r| {
            r.predicate == RelationPredicate::Contains
                && r.subject == RelationEndpoint::Entity(owner.id)
                && r.object == RelationEndpoint::Entity(method.id)
        })
        .collect();
    assert_eq!(contains.len(), 1);
    let evidence = contains[0].evidence.source.as_ref().unwrap();
    assert_eq!(evidence.content_hash(), content_hash(raw));
    assert!(evidence.span().start_byte() <= 59 && evidence.span().end_byte() >= 65);
    assert!(doc.skipped_regions.iter().any(|gap| {
        gap.detail == "abap-method-parameter-and-contract-bindings-unavailable"
            && gap.source.span().start_byte() == 65
            && gap.source.span().end_byte() > 65
    }));
    assert_eq!(
        doc.occurrences
            .iter()
            .filter(|s| s.role == OccurrenceRole::Definition)
            .count(),
        2
    );
    assert!(
        doc.occurrences
            .iter()
            .all(|site| site.role == OccurrenceRole::Definition)
    );
    assert!(!doc.entities.iter().any(|e| e.kind == EntityKind::Parameter));
}

#[test]
fn mixed_case_names_keep_folded_identity_and_authored_display() {
    let raw = b"iNtErFaCe ZiF_Mixed PuBlIc. mEtHoDs ReNdEr. eNdInTeRfAcE.";
    let output = output(raw);
    let doc = output.document();
    let lowercase_raw = b"interface zif_mixed public. methods render. endinterface.";
    let lowercase = self::output(lowercase_raw);
    for (kind, canonical, display) in [
        (EntityKind::Interface, "zif_mixed", "ZiF_Mixed"),
        (EntityKind::Method, "render", "ReNdEr"),
    ] {
        let entity = doc.entities.iter().find(|e| e.kind == kind).unwrap();
        assert_eq!(entity.canonical_name, canonical);
        assert_eq!(entity.display_name, display);
        let equivalent = lowercase
            .document()
            .entities
            .iter()
            .find(|e| e.kind == kind)
            .unwrap();
        assert_eq!(
            entity.id, equivalent.id,
            "case-only edit keeps the native declaration identity"
        );
        assert_eq!(equivalent.canonical_name, canonical);
        assert_eq!(equivalent.display_name, canonical);
        let definition = doc
            .occurrences
            .iter()
            .find(|site| site.target == OccurrenceTarget::Resolved { symbol: entity.id })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            raw.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(display.as_bytes())
        );
        assert_eq!(
            definition.syntactic_text_hash,
            content_hash(display.as_bytes())
        );
        assert_eq!(definition.source.content_hash(), content_hash(raw));
        let equivalent_definition = lowercase
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: equivalent.id,
                        }
            })
            .unwrap();
        assert_eq!(
            equivalent_definition.syntactic_text_hash,
            content_hash(canonical.as_bytes())
        );
        assert_eq!(
            equivalent_definition.source.content_hash(),
            content_hash(lowercase_raw)
        );
    }
}

#[test]
fn opaque_or_recovered_abap_cannot_donate_interface_or_method_bindings() {
    for raw in [
        "* INTERFACE hidden.\n\" METHODS stolen.\nWRITE 'INTERFACE fake.'.",
        "frobnicate missing_target( value ).",
        "INTERFACE zif_missing. METHODS stolen.",
        "CLASS zcl_hidden DEFINITION. PUBLIC SECTION. METHODS stolen. ENDCLASS.",
        "CLASS zcl_hidden IMPLEMENTATION. METHOD stolen. ENDMETHOD. ENDCLASS.",
        "INTERFACE zif_forward DEFERRED.",
    ] {
        let output = output(raw.as_bytes());
        let doc = output.document();
        assert!(
            doc.entities.iter().all(|e| e.kind == EntityKind::Module),
            "{raw}: {:?}",
            doc.entities
        );
        assert!(
            doc.occurrences
                .iter()
                .all(|site| site.role == OccurrenceRole::Documentation),
            "{raw}: {:?}",
            doc.occurrences
        );
        let mut comments = Vec::new();
        for site in &doc.occurrences {
            assert_eq!(site.syntax_kind, "abap.comment.comment");
            assert_eq!(site.provenance, doc.provenance[0].id);
            assert_eq!(doc.provenance[0].language, "abap");
            assert_eq!(site.source.content_hash(), content_hash(raw.as_bytes()));
            assert_eq!(site.evidence.source.as_ref(), Some(&site.source));
            let span = site.source.span();
            comments.push((span.start_byte(), span.end_byte()));
            let text = raw
                .as_bytes()
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                )
                .unwrap();
            assert!(text == b"* INTERFACE hidden." || text == b"\" METHODS stolen.");
            assert_eq!(site.syntactic_text_hash, content_hash(text));
        }
        comments.sort_unstable();
        assert_eq!(
            comments,
            if raw.starts_with('*') {
                vec![(0, 19), (20, 37)]
            } else {
                Vec::new()
            }
        );
        assert!(
            doc.skipped_regions
                .iter()
                .any(|gap| gap.detail.starts_with("abap-")),
            "{raw}"
        );
    }
}

#[test]
fn same_folded_method_names_keep_their_distinct_native_interface_owners() {
    let raw = b"INTERFACE first. METHODS ReNdEr. ENDINTERFACE. INTERFACE second. METHODS render. ENDINTERFACE.";
    let output = output(raw);
    let doc = output.document();
    let methods: Vec<_> = doc
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::Method)
        .collect();
    assert_eq!(methods.len(), 2);
    assert_ne!(methods[0].id, methods[1].id);
    for name in ["first", "second"] {
        let owner = doc
            .entities
            .iter()
            .find(|e| e.kind == EntityKind::Interface && e.canonical_name == name)
            .unwrap();
        let children: Vec<_> = methods
            .iter()
            .filter(|m| m.container == Some(rootlight_ir::ContainerRef::Entity(owner.id)))
            .collect();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].canonical_name, "render");
        assert_eq!(
            doc.relations
                .iter()
                .filter(|r| {
                    r.predicate == RelationPredicate::Contains
                        && r.subject == RelationEndpoint::Entity(owner.id)
                        && r.object == RelationEndpoint::Entity(children[0].id)
                })
                .count(),
            1
        );
    }
}
