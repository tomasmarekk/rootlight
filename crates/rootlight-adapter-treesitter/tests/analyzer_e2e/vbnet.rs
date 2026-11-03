//! Original VB.NET declarations retain exact BOM/CRLF evidence and native owners.
//! Type resolution, dispatch and body references remain explicit coverage gaps.

use super::*;

const VBNET: LanguageCase = LanguageCase {
    name: "visual-basic-dotnet",
    path: "native/original.vb",
    frontend: "tree-sitter-vbnet-0.3.0",
    source: include_str!("../../../../tests/native-grammars/fixtures/vbnet/TaskbarAnimate.vb"),
    generated: false,
    body_before: "Main.ClearMemory()",
    body_after: "Main.ClearMemory( )",
};

fn output(raw: &[u8]) -> AnalysisOutput {
    output_for(VBNET, raw)
}

fn output_for(case: LanguageCase, raw: &[u8]) -> AnalysisOutput {
    let native = Arc::new(provider());
    let analyzer = analyzer(&native, case);
    let fixture = Fixture::new(case, raw);
    analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, case, &limits()),
        &ExtensionSupport::default(),
    )
}

#[test]
fn vbnet_attributes_preserve_native_definitions_spans_and_nested_owners() {
    let raw = b"Public Class Owner\n<StructLayout(LayoutKind.Sequential)>\nPublic Class Child\nPublic Value As Integer\nEnd Class\nPublic Shared Function Check(ByVal handle As Integer) As <MarshalAs(UnmanagedType.Bool)> Boolean\nReturn True\nEnd Function\nEnd Class\n";
    let output = output(raw);
    let doc = output.document();
    assert_eq!(doc.files[0].content_hash, content_hash(raw));
    let owner = symbol_id_named(doc, "Owner");
    let method = symbol_id_named(doc, "Check");
    let module = doc
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module)
        .unwrap();
    let module_source = module.evidence.source.as_ref().unwrap();
    assert_eq!(module_source.span().file(), doc.files[0].id);
    assert_eq!(module_source.generation(), doc.generation);
    assert_eq!(module_source.content_hash(), content_hash(raw));
    for (name, kind, start, end, container) in [
        ("Owner", EntityKind::Class, 13, 18, Some(module.id)),
        ("Child", EntityKind::Class, 70, 75, Some(owner)),
        ("Check", EntityKind::Method, 133, 138, Some(owner)),
        ("handle", EntityKind::Parameter, 145, 151, Some(method)),
    ] {
        let entity = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, kind);
        assert_eq!(
            entity.container,
            container.map(rootlight_ir::ContainerRef::Entity)
        );
        let sites: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(sites.len(), 1);
        assert_eq!(
            (
                sites[0].source.span().start_byte(),
                sites[0].source.span().end_byte()
            ),
            (start, end)
        );
        assert_eq!(sites[0].source.content_hash(), content_hash(raw));
        assert_eq!(sites[0].source.span().file(), doc.files[0].id);
        assert_eq!(sites[0].source.generation(), doc.generation);
        assert_eq!(
            doc.relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(container.unwrap())
                    && relation.object == RelationEndpoint::Entity(entity.id))
                .count(),
            1
        );
    }
    assert!(
        !doc.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
    );
    assert!(!doc.entities.iter().any(|entity| matches!(
        entity.canonical_name.as_str(),
        "StructLayout" | "LayoutKind" | "Sequential" | "MarshalAs" | "UnmanagedType" | "Bool"
    )));
    assert!(!doc.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::Calls | RelationPredicate::Reads | RelationPredicate::Writes
    )));
    assert!(
        doc.skipped_regions
            .iter()
            .any(|gap| gap.detail == "vbnet-statement-binding-and-call-analysis-unavailable")
    );
}

#[test]
fn vbnet_original_main_window_recovers_native_owners_without_body_bindings() {
    let case = LanguageCase {
        path: "native/MainWindow.xaml.vb",
        source: include_str!("../../../../tests/native-grammars/fixtures/vbnet/MainWindow.xaml.vb"),
        ..VBNET
    };
    let raw = case.source.as_bytes();
    assert_eq!(raw.len(), 93_650);
    // The complete original exceeds the snippet tree ceiling; output quotas stay fixed.
    let node_limit = 128 * 1024;
    let base = limits();
    let original_limits = AnalysisLimits::new(
        base.max_source_bytes(),
        node_limit,
        base.max_syntax_depth(),
        base.max_embedded_ranges(),
        base.max_reported_memory_bytes(),
        base.syntax_stream().clone(),
        base.ir_stream().clone(),
        base.ir().clone(),
    )
    .unwrap();
    let native = Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                MAX_SOURCE_BYTES,
                node_limit,
                MAX_SYNTAX_DEPTH,
                32,
                64,
                1,
                16 * 1024 * 1024,
                ParserSettings::new(4096).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let analyzer = analyzer(&native, case);
    let fixture = Fixture::new(case, raw);
    let output = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, case, &original_limits),
        &ExtensionSupport::default(),
    );
    let doc = output.document();
    assert_eq!(doc.files[0].content_hash, content_hash(raw));
    for (kind, count) in [
        (EntityKind::Class, 1),
        (EntityKind::Struct, 3),
        (EntityKind::Enum, 3),
        (EntityKind::Method, 48),
    ] {
        assert_eq!(
            doc.entities.iter().filter(|e| e.kind == kind).count(),
            count,
            "{kind:?}: gaps={:?}",
            doc.skipped_regions
                .iter()
                .map(|gap| gap.detail.as_str())
                .take(8)
                .collect::<Vec<_>>()
        );
    }
    let class = symbol_id_named(doc, "MainWindow");
    for (name, kind, start, end) in [
        ("MainWindow", EntityKind::Class, 336, 346),
        ("AccentPolicy", EntityKind::Struct, 3369, 3381),
        ("Button_Click_8", EntityKind::Method, 71_771, 71_785),
    ] {
        let entity = doc
            .entities
            .iter()
            .find(|e| e.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, kind);
        if entity.id != class {
            assert_eq!(
                entity.container,
                Some(rootlight_ir::ContainerRef::Entity(class))
            );
        }
        let sites: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(sites.len(), 1);
        assert_eq!(
            (
                sites[0].source.span().start_byte(),
                sites[0].source.span().end_byte()
            ),
            (start, end)
        );
        assert_eq!(sites[0].source.content_hash(), content_hash(raw));
    }
    for start in [73_925, 74_403] {
        assert!(doc.skipped_regions.iter().any(|gap| {
            gap.detail == "vbnet-statement-binding-and-call-analysis-unavailable"
                && gap.source.span().start_byte() == start
        }));
    }
    assert!(
        !doc.skipped_regions
            .iter()
            .any(|gap| { gap.detail == "vbnet-complete-native-structure-unavailable" })
    );
    assert!(!doc.entities.iter().any(|entity| {
        matches!(
            entity.canonical_name.as_str(),
            "StructLayout" | "LayoutKind" | "Sequential" | "End"
        )
    }));
    assert!(!doc.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::Calls | RelationPredicate::Reads | RelationPredicate::Writes
    )));
}

#[test]
fn vbnet_complete_original_external_methods_have_authored_names_and_owners() {
    for (case, names, spans, method_count) in [
        (
            LanguageCase {
                path: "native/MSAA.vb",
                source: include_str!("../../../../tests/native-grammars/fixtures/vbnet/MSAA.vb"),
                ..VBNET
            },
            ["MSAA", "AccessibleObjectFromWindow", "Hwnd"],
            [(122, 126), (607, 633), (654, 658)],
            7,
        ),
        (
            LanguageCase {
                path: "native/TaskbarStyle.vb",
                source: include_str!(
                    "../../../../tests/native-grammars/fixtures/vbnet/TaskbarStyle.vb"
                ),
                ..VBNET
            },
            ["TaskbarStyle", "EnumWindows", "Adress"],
            [(149, 161), (292, 303), (324, 330)],
            8,
        ),
    ] {
        let output = output_for(case, case.source.as_bytes());
        let doc = output.document();
        assert_eq!(doc.files[0].language, "visual-basic-dotnet");
        assert_eq!(
            doc.entities
                .iter()
                .filter(|e| e.kind == EntityKind::Method)
                .count(),
            method_count
        );
        let mut selected = Vec::new();
        for ((kind, name), (start, end)) in
            [EntityKind::Class, EntityKind::Method, EntityKind::Parameter]
                .into_iter()
                .zip(names)
                .zip(spans)
        {
            let entity = doc
                .entities
                .iter()
                .find(|e| e.kind == kind && e.canonical_name == name)
                .expect(name);
            assert_eq!(entity.language, "visual-basic-dotnet");
            assert_eq!(entity.display_name, name);
            let definition = doc
                .occurrences
                .iter()
                .find(|o| {
                    o.role == OccurrenceRole::Definition
                        && o.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .unwrap();
            assert_eq!(
                (
                    definition.source.span().start_byte(),
                    definition.source.span().end_byte()
                ),
                (start, end)
            );
            assert_eq!(
                &case.source.as_bytes()
                    [usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
                name.as_bytes()
            );
            selected.push(entity);
        }
        for pair in selected.windows(2) {
            assert_eq!(
                pair[1].container,
                Some(rootlight_ir::ContainerRef::Entity(pair[0].id))
            );
            assert!(
                doc.relations
                    .iter()
                    .any(|r| r.predicate == RelationPredicate::Contains
                        && r.subject == RelationEndpoint::Entity(pair[0].id)
                        && r.object == RelationEndpoint::Entity(pair[1].id))
            );
        }
        assert!(
            doc.skipped_regions
                .iter()
                .any(|g| g.detail == "vbnet-external-abi-and-entry-point-bindings-unavailable")
        );
        assert!(
            !doc.skipped_regions
                .iter()
                .any(|g| g.detail == "vbnet-complete-native-structure-unavailable")
        );
        assert!(
            !doc.relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Calls)
        );
    }
}

#[test]
fn vbnet_external_alias_literals_do_not_become_declared_names_or_bindings() {
    let source = b"Class Api\nDeclare Unicode Function Written Lib \"Native.DLL\" Alias \"ExportW\"(ByRef VaLuE As Integer) As Integer\nEnd Class\n";
    let result = output(source);
    let folded = output(&source.to_ascii_lowercase());
    for (kind, name) in [
        (EntityKind::Method, "Written"),
        (EntityKind::Parameter, "VaLuE"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|e| e.kind == kind)
            .unwrap();
        let other = folded
            .document()
            .entities
            .iter()
            .find(|e| e.kind == kind)
            .unwrap();
        assert_eq!(entity.canonical_name, name);
        assert_eq!(entity.id, other.id);
    }
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|e| matches!(e.canonical_name.as_str(), "Native.DLL" | "ExportW"))
    );
    for source in [
        "Class Api\nDeclare Sub Broken Lib \"n.dll\"() As Integer\nEnd Class\n",
        "#If False Then\nClass Api\nDeclare Sub Hidden Lib \"n.dll\"()\nEnd Class\n#End If\n",
        "Interface Api\nDeclare Sub Broken Lib \"n.dll\"()\nEnd Interface\n",
    ] {
        let result = output(source.as_bytes());
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|e| matches!(e.kind, EntityKind::Method | EntityKind::Parameter))
        );
    }
}

#[test]
fn original_vbnet_class_method_and_formals_have_exact_native_definitions() {
    let raw = VBNET.source.as_bytes();
    assert_eq!(raw.len(), 4124);
    let output = output(raw);
    let doc = output.document();
    assert_eq!(doc.files[0].language, "visual-basic-dotnet");
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
    assert_eq!(class.canonical_name, "TaskbarAnimate");
    assert_eq!(class.display_name, "TaskbarAnimate");
    assert_eq!(method.canonical_name, "Animate");
    assert_eq!(method.display_name, "Animate");
    assert_eq!(
        doc.entities
            .iter()
            .filter(|e| e.kind == EntityKind::Parameter)
            .count(),
        8
    );
    // These literals are the original corpus obligations, independent of registry IDs.
    for (entity, kind, name, start, end) in [
        (class, EntityKind::Class, "TaskbarAnimate", 36, 50),
        (method, EntityKind::Method, "Animate", 122, 129),
        (formal, EntityKind::Parameter, "hwnd", 136, 140),
    ] {
        assert_eq!(entity.language, "visual-basic-dotnet");
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.canonical_name, name);
        assert_eq!(entity.display_name, name);
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
    let ownership = doc
        .relations
        .iter()
        .find(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(class.id)
                && relation.object == RelationEndpoint::Entity(method.id)
        })
        .unwrap();
    let source = ownership.evidence.source.as_ref().unwrap();
    assert!(source.span().start_byte() <= 122);
    assert!(source.span().end_byte() >= 129);
    assert_eq!(source.content_hash(), content_hash(raw));
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
    let raw = b"nAmEsPaCe OuTeR\npUbLiC cLaSs OwNeR\nPuBlIc SuB RuN(ByVal VaLuE As Integer)\nEnD SuB\nEnD ClAsS\nEnD nAmEsPaCe\n";
    let output = output(raw);
    let lower = self::output(&raw.to_ascii_lowercase());
    for (kind, display) in [
        (EntityKind::Namespace, "OuTeR"),
        (EntityKind::Class, "OwNeR"),
        (EntityKind::Method, "RuN"),
        (EntityKind::Parameter, "VaLuE"),
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
        assert_eq!(entity.language, "visual-basic-dotnet");
        assert_eq!(entity.canonical_name, display);
        assert_eq!(entity.display_name, display);
        assert!(entity.qualified_name.ends_with(display));
        assert_eq!(equivalent.canonical_name, display.to_ascii_lowercase());
    }
    let formal = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter)
        .unwrap();
    assert!(formal.qualified_name.ends_with("OuTeR::OwNeR::RuN::VaLuE"));
}

#[test]
fn vbnet_unsupported_contexts_do_not_fabricate_declarations_or_bindings() {
    for raw in [
        "Public Class Broken\nPublic Sub Run(ByVal value As Integer)\n",
        "Class Broken\nSub Run()\nEnd\nEnd Class\n",
        "Class Broken\nSub Run()\n<A>\nStructure Invalid\nEnd Structure\nEnd Sub\nEnd Class\n",
        "Class Broken\n<Assembly: Marker>\nStructure Invalid\nEnd Structure\nEnd Class\n",
        "Class Broken\n<Module: Marker>\nStructure Invalid\nEnd Structure\nEnd Class\n",
        "Class Broken\nSub Run()\n<Marker>\nClass Invalid\nEnd Class\nEnd Sub\nEnd Class\n",
        "Class Broken\n<Assembly: Marker>\nClass Invalid\nEnd Class\nEnd Class\n",
        "Class Broken\n<Module: Marker>\nClass Invalid\nEnd Class\nEnd Class\n",
        "Class Broken\nFunction Read() As <Return: Marker> Boolean\nEnd Function\nEnd Class\n",
        "Class Broken\nFunction Read() As <Marker>: Boolean\nEnd Function\nEnd Class\n",
        "Class Broken\nProperty Value As <Marker>: Integer\nEnd Class\n",
        "Class Broken\n<Marker>: Class Invalid\nEnd Class\nEnd Class\n",
        "Class Broken\nPublic Value As <Marker> Boolean\nEnd Class\n",
        "#If False Then\nPublic Class ConditionalOwner\nEnd Class\n#End If\n",
        "Namespace Unproved.Path\nPublic Class Hidden\nEnd Class\nEnd Namespace\n",
    ] {
        let output = output(raw.as_bytes());
        assert!(
            !output.document().entities.iter().any(|e| matches!(
                e.kind,
                EntityKind::Class | EntityKind::Struct | EntityKind::Method | EntityKind::Parameter
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
            .find(|e| e.kind == kind && e.canonical_name == "Owner")
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
            .any(|e| e.kind == EntityKind::Enum && e.canonical_name == "Owner")
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|g| g.detail == "vbnet-native-construct-not-lowered")
    );
}
