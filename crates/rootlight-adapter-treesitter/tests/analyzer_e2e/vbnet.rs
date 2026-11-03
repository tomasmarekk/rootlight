//! Original VB.NET declarations retain exact BOM/CRLF evidence and native owners.
//! Type resolution, dispatch and body references remain explicit coverage gaps.

use super::*;

const VBNET: LanguageCase = LanguageCase {
    name: "vbnet",
    path: "native/original.vb",
    frontend: "tree-sitter-vbnet-0.3.0",
    source: include_str!("../../../../tests/native-grammars/fixtures/vbnet/TaskbarAnimate.vb"),
    generated: false,
    body_before: "Main.ClearMemory()",
    body_after: "Main.ClearMemory( )",
};

fn output(raw: &[u8]) -> AnalysisOutput {
    let native = Arc::new(provider());
    let analyzer = analyzer(&native, VBNET);
    let fixture = Fixture::new(VBNET, raw);
    analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, VBNET, &limits()),
        &ExtensionSupport::default(),
    )
}

#[test]
fn original_vbnet_class_method_and_formals_have_exact_native_definitions() {
    let raw = VBNET.source.as_bytes();
    assert_eq!(raw.len(), 4124);
    let output = output(raw);
    let doc = output.document();
    assert_eq!(doc.files[0].language, "vbnet");
    assert_eq!(doc.files[0].content_hash, content_hash(raw));
    let class = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Class)
        .expect("original native class");
    let method = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Method)
        .expect("original native method");
    let formal = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Parameter && e.canonical_name == "hwnd")
        .expect("original native formal");
    assert_eq!(class.canonical_name, "taskbaranimate");
    assert_eq!(class.display_name, "TaskbarAnimate");
    assert_eq!(method.canonical_name, "animate");
    assert_eq!(method.display_name, "Animate");
    assert_eq!(
        doc.entities
            .iter()
            .filter(|e| e.kind == EntityKind::Parameter)
            .count(),
        8
    );
    for (entity, (start, end)) in [
        (class, (36, 50)),
        (method, (122, 129)),
        (formal, (136, 140)),
    ] {
        let sites: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|o| {
                o.role == OccurrenceRole::Definition
                    && o.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(sites.len(), 1, "{:?}", doc.skipped_regions);
        let site = sites[0];
        assert_eq!(
            (
                site.source.span().start_byte(),
                site.source.span().end_byte()
            ),
            (start, end)
        );
        assert_eq!(site.source.content_hash(), content_hash(raw));
        assert_eq!(
            site.syntactic_text_hash,
            content_hash(&raw[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()])
        );
    }
    for (owner, member) in [(class, method), (method, formal)] {
        assert_eq!(
            member.container,
            Some(rootlight_ir::ContainerRef::Entity(owner.id))
        );
        assert_eq!(
            doc.relations
                .iter()
                .filter(|r| r.predicate == RelationPredicate::Contains
                    && r.subject == RelationEndpoint::Entity(owner.id)
                    && r.object == RelationEndpoint::Entity(member.id))
                .count(),
            1
        );
    }
    assert!(
        doc.skipped_regions
            .iter()
            .any(|g| g.detail == "vbnet-statement-binding-and-call-analysis-unavailable")
    );
    assert!(!doc.relations.iter().any(|r| matches!(
        r.predicate,
        RelationPredicate::Calls
            | RelationPredicate::Reads
            | RelationPredicate::Writes
            | RelationPredicate::Extends
            | RelationPredicate::Implements
    )));
}

#[test]
fn vbnet_case_identity_preserves_written_names_and_native_owners() {
    let raw = b"pUbLiC cLaSs OwNeR\nPuBlIc SuB RuN(ByVal VaLuE As Integer)\nEnD SuB\nEnD ClAsS\n";
    let output = output(raw);
    let lower = self::output(&raw.to_ascii_lowercase());
    for (kind, name, display) in [
        (EntityKind::Class, "owner", "OwNeR"),
        (EntityKind::Method, "run", "RuN"),
        (EntityKind::Parameter, "value", "VaLuE"),
    ] {
        let entity = output
            .document()
            .entities
            .iter()
            .find(|e| e.kind == kind)
            .unwrap();
        let equivalent = lower
            .document()
            .entities
            .iter()
            .find(|e| e.kind == kind)
            .unwrap();
        assert_eq!(entity.id, equivalent.id);
        assert_eq!(entity.canonical_name, name);
        assert_eq!(entity.display_name, display);
    }
}

#[test]
fn vbnet_unsupported_contexts_do_not_fabricate_declarations_or_bindings() {
    for raw in [
        "Public Class Broken\nPublic Sub Run(ByVal value As Integer)\n",
        "#If False Then\nPublic Class ConditionalOwner\nEnd Class\n#End If\n",
        "Namespace Unproved.Path\nPublic Class Hidden\nEnd Class\nEnd Namespace\n",
    ] {
        let output = output(raw.as_bytes());
        assert!(
            !output.document().entities.iter().any(|e| matches!(
                e.kind,
                EntityKind::Class | EntityKind::Method | EntityKind::Parameter
            )),
            "{raw}: {:?}",
            output.document().entities
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|g| g.detail.starts_with("vbnet-")),
            "{raw}"
        );
    }
    let raw = b"Public Class Owner\nPublic Sub Run(ByVal value As Integer)\nDim text As String = \"#If False Then Class Fake\"\n' Public Class Hidden\nEnd Sub\nEnd Class\n";
    let output = output(raw);
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::Class)
            .count(),
        1
    );
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::Method)
            .count(),
        1
    );
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::Parameter)
            .count(),
        1
    );
}

#[test]
fn vbnet_nested_native_owners_keep_class_and_method_containment() {
    let raw = b"Namespace Outer\nPublic Class Owner\nPublic Sub Run(ByVal value As Integer)\nEnd Sub\nEnd Class\nEnd Namespace\n";
    let output = output(raw);
    let doc = output.document();
    let namespace = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Namespace)
        .unwrap();
    let class = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Class)
        .unwrap();
    let method = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Method)
        .unwrap();
    assert_eq!(
        class.container,
        Some(rootlight_ir::ContainerRef::Entity(namespace.id))
    );
    assert_eq!(
        method.container,
        Some(rootlight_ir::ContainerRef::Entity(class.id))
    );
}

#[test]
fn vbnet_native_type_containers_keep_direct_method_and_formal_ownership() {
    for (source, kind) in [
        (
            "Public Module Owner\nPublic Sub Run(ByVal value As Integer)\nEnd Sub\nEnd Module\n",
            EntityKind::Module,
        ),
        (
            "Public Structure Owner\nPublic Sub Run(ByVal value As Integer)\nEnd Sub\nEnd Structure\n",
            EntityKind::Struct,
        ),
        (
            "Public Interface Owner\nSub Run(ByVal value As Integer)\nEnd Interface\n",
            EntityKind::Interface,
        ),
    ] {
        let output = output(source.as_bytes());
        let doc = output.document();
        let owner = doc
            .entities
            .iter()
            .find(|e| e.kind == kind && e.canonical_name == "owner")
            .unwrap();
        let method = doc
            .entities
            .iter()
            .find(|e| e.kind == EntityKind::Method)
            .unwrap();
        let parameter = doc
            .entities
            .iter()
            .find(|e| e.kind == EntityKind::Parameter)
            .unwrap();
        assert_eq!(
            method.container,
            Some(rootlight_ir::ContainerRef::Entity(owner.id))
        );
        assert_eq!(
            parameter.container,
            Some(rootlight_ir::ContainerRef::Entity(method.id))
        );
    }
    let output = output(b"Public Enum Owner\nFirstValue\nEnd Enum\n");
    assert!(
        output
            .document()
            .entities
            .iter()
            .any(|e| e.kind == EntityKind::Enum && e.canonical_name == "owner")
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|g| g.detail == "vbnet-native-construct-not-lowered")
    );
}
