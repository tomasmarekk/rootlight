//! Pascal written declarations through the public analyzer and validated IR.
//! Native field owners and byte-exact headers do not establish compiler,
//! overload, remote-type, accessor or preprocessor bindings.

use super::*;

const PASCAL: LanguageCase = LanguageCase {
    name: "pascal",
    path: "src/definitions.pas",
    frontend: "tree-sitter-pascal-0.10.2",
    source: "procedure Adjust(Value: Integer); begin Value := Value+1; end;\n",
    generated: false,
    body_before: "Value+1",
    body_after: "Value+2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(PASCAL, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, PASCAL),
        &request(&fixture.snapshot, &fixture.source, PASCAL, &budget),
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
    let occurrences = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
        })
        .collect::<Vec<_>>();
    assert_eq!(occurrences.len(), 1, "{name}");
    assert_eq!(text(source, &occurrences[0].source), name);
    assert_eq!(
        occurrences[0].syntactic_text_hash,
        content_hash(name.as_bytes())
    );
    assert_eq!(
        occurrences[0].source.generation(),
        result.document().generation
    );
    assert_eq!(
        occurrences[0].source.content_hash(),
        content_hash(source.as_bytes())
    );
}

fn signatures(result: &AnalysisOutput, entity: &rootlight_ir::EntityRecord) -> Vec<String> {
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

fn assert_owner(result: &AnalysisOutput, child: &str, owner: &str) {
    assert_eq!(
        named(result, child).container,
        Some(rootlight_ir::ContainerRef::Entity(named(result, owner).id)),
        "{child}"
    );
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
            .any(|gap| gap.detail == "pascal-compiler-and-binding-semantics-unavailable")
    );
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.syntax_kind.starts_with("pascal.")
                && occurrence.role != OccurrenceRole::Definition)
            .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn pascal_unit_forward_types_grouped_fields_and_property_formals_keep_direct_owners() {
    let source = "unit ShapeStore;\ninterface\ntype\n\
        TFuture = class;\n\
        IValue = interface function Read(const Key: Integer): Integer; end;\n\
        TBox = class\nprivate\nLeft, Right: Integer;\npublic\n\
        constructor Create(Start: Integer);\n\
        property Items[Index: Integer]: Integer read Left write Right;\n\
        end;\nimplementation\nend.";
    let result = output(source);
    for (name, kind) in [
        ("ShapeStore", EntityKind::Module),
        ("TFuture", EntityKind::Class),
        ("IValue", EntityKind::Interface),
        ("TBox", EntityKind::Class),
        ("Read", EntityKind::Method),
        ("Key", EntityKind::Parameter),
        ("Left", EntityKind::Field),
        ("Right", EntityKind::Field),
        ("Create", EntityKind::Constructor),
        ("Start", EntityKind::Parameter),
        ("Items", EntityKind::Property),
        ("Index", EntityKind::Parameter),
    ] {
        assert_definition(&result, source, name, kind);
    }
    for (child, parent) in [
        ("Read", "IValue"),
        ("Key", "Read"),
        ("Left", "TBox"),
        ("Right", "TBox"),
        ("Create", "TBox"),
        ("Start", "Create"),
        ("Items", "TBox"),
        ("Index", "Items"),
    ] {
        assert_owner(&result, child, parent);
    }
    assert_eq!(
        signatures(&result, named(&result, "ShapeStore")),
        ["unit ShapeStore;"]
    );
    assert_eq!(
        signatures(&result, named(&result, "Read")),
        ["function Read(const Key: Integer): Integer;"]
    );
    assert_eq!(
        signatures(&result, named(&result, "Items")),
        ["property Items[Index: Integer]: Integer read Left write Right;"]
    );
    for name in ["Left", "Right"] {
        assert!(signatures(&result, named(&result, name)).is_empty());
        assert_eq!(
            text(
                source,
                named(&result, name).evidence.source.as_ref().unwrap()
            ),
            name
        );
    }
    assert_ne!(named(&result, "Left").id, named(&result, "Right").id);
    assert_eq!(
        text(
            source,
            named(&result, "TFuture").evidence.source.as_ref().unwrap()
        ),
        "TFuture = class;"
    );
    for accessor in ["Left", "Right"] {
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role != OccurrenceRole::Definition
                    && text(source, &occurrence.source) == accessor
                    && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
        );
    }
    assert_unqualified(&result);
}

#[test]
fn pascal_qualified_implementations_do_not_invent_remote_type_or_prototype_binding() {
    let source = "unit RoutineScopes;\ninterface\ntype TCounter = class\n\
        function Map(Value: Integer): Integer; overload;\n\
        function Map(Value: String): String; overload;\nend;\nimplementation\n\
        function TCounter.Map(Value: Integer): Integer;\nvar Left, Right: Integer;\n\
        function Local(const Input, Scale: Integer): Integer;\n\
        begin Local := Input + Scale; end;\n\
        begin Map := Local(Value, 1); end;\n\
        function TCounter.Map(Value: String): String;\nbegin Map := Value; end;\nend.";
    let result = output(source);
    let maps = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "Map")
        .collect::<Vec<_>>();
    assert_eq!(
        maps.len(),
        4,
        "written overload and implementation sites must remain distinct: {maps:?}"
    );
    let ids = maps.iter().map(|entity| entity.id).collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), 4);
    let mut implementation = None;
    for entity in maps {
        let header = signatures(&result, entity);
        assert_eq!(header.len(), 1);
        if header[0].starts_with("function TCounter.Map") {
            assert_eq!(entity.kind, EntityKind::Function);
            assert_eq!(
                entity.container,
                Some(rootlight_ir::ContainerRef::Entity(
                    named(&result, "RoutineScopes").id
                ))
            );
            if header[0] == "function TCounter.Map(Value: Integer): Integer;" {
                implementation = Some(entity);
            }
        } else {
            assert_eq!(entity.kind, EntityKind::Method);
            assert_eq!(
                entity.container,
                Some(rootlight_ir::ContainerRef::Entity(
                    named(&result, "TCounter").id
                ))
            );
        }
        let definitions = result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(text(source, &definitions[0].source), "Map");
    }
    let implementation = implementation.unwrap();
    assert_eq!(
        named(&result, "Local").container,
        Some(rootlight_ir::ContainerRef::Entity(implementation.id))
    );
    for name in ["Left", "Right"] {
        assert_definition(&result, source, name, EntityKind::Variable);
        assert_eq!(
            named(&result, name).container,
            Some(rootlight_ir::ContainerRef::Entity(implementation.id))
        );
        assert!(signatures(&result, named(&result, name)).is_empty());
    }
    for name in ["Input", "Scale"] {
        assert_definition(&result, source, name, EntityKind::Parameter);
        assert_owner(&result, name, "Local");
        assert!(signatures(&result, named(&result, name)).is_empty());
        assert_eq!(
            text(
                source,
                named(&result, name).evidence.source.as_ref().unwrap()
            ),
            name
        );
    }
    assert_eq!(
        signatures(&result, named(&result, "Local")),
        ["function Local(const Input, Scale: Integer): Integer;"]
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "TCounter.Map")
    );
    assert_unqualified(&result);
}

#[test]
fn pascal_bom_crlf_escaped_case_and_literal_controls_preserve_original_bytes() {
    let source = "\u{feff}program RawCase;\r\n\
        { procedure Hidden; begin end; }\r\n\
        (* type FakeClass = class end; *)\r\n\
        // function Shadow: Integer; begin end;\r\n\
        const Caption = 'λ procedure Quoted; begin end;';\r\n\
        var MixedCase: Integer;\r\n\
        procedure Written(const &type: Integer);\r\nbegin MixedCase := &type; end;\r\n\
        begin Written(1); end.";
    let result = output(source);
    for (name, kind) in [
        ("RawCase", EntityKind::Module),
        ("MixedCase", EntityKind::Variable),
        ("Written", EntityKind::Function),
        ("&type", EntityKind::Parameter),
    ] {
        assert_definition(&result, source, name, kind);
    }
    assert_owner(&result, "&type", "Written");
    assert_eq!(
        signatures(&result, named(&result, "Written")),
        ["procedure Written(const &type: Integer);"]
    );
    assert_eq!(
        text(
            source,
            named(&result, "Written").evidence.source.as_ref().unwrap()
        ),
        "procedure Written(const &type: Integer);\r\nbegin MixedCase := &type; end;"
    );
    for false_name in [
        "Hidden",
        "FakeClass",
        "Shadow",
        "Quoted",
        "type",
        "rawcase",
        "mixedcase",
    ] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == false_name),
            "{false_name}"
        );
    }
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role != OccurrenceRole::Definition
                && text(source, &occurrence.source) == "&type"
                && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert_unqualified(&result);
}

#[test]
fn pascal_malformed_header_cannot_reparent_formals_into_a_clean_following_routine() {
    let source = "procedure Damaged(Value: );\nbegin end;\n\
        procedure Stable(Argument: Integer);\nbegin end;";
    let result = output(source);
    assert_definition(&result, source, "Stable", EntityKind::Function);
    assert_definition(&result, source, "Argument", EntityKind::Parameter);
    assert_owner(&result, "Argument", "Stable");
    assert_eq!(
        signatures(&result, named(&result, "Stable")),
        ["procedure Stable(Argument: Integer);"]
    );
    for name in ["Damaged", "Value"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "pascal-source-identity-unavailable")
    );
    assert_unqualified(&result);
}

#[test]
fn pascal_library_record_and_class_variables_keep_written_type_boundaries() {
    let source = "library NativeShapes;\ntype\n\
        TEntry = record Left, Right: Integer; end;\n\
        TState = class class var Total: Integer; end;\n\
        var Current: TEntry;\nbegin end.";
    let result = output(source);
    for (name, kind) in [
        ("NativeShapes", EntityKind::Module),
        ("TEntry", EntityKind::Struct),
        ("TState", EntityKind::Class),
        ("Left", EntityKind::Field),
        ("Right", EntityKind::Field),
        ("Total", EntityKind::Field),
        ("Current", EntityKind::Variable),
    ] {
        assert_definition(&result, source, name, kind);
    }
    assert_owner(&result, "Left", "TEntry");
    assert_owner(&result, "Right", "TEntry");
    assert_owner(&result, "Total", "TState");
    assert_eq!(
        signatures(&result, named(&result, "NativeShapes")),
        ["library NativeShapes;"]
    );
    for name in ["Left", "Right"] {
        assert!(signatures(&result, named(&result, name)).is_empty());
    }
    assert_eq!(
        signatures(&result, named(&result, "Total")),
        ["Total: Integer;"]
    );
    assert_unqualified(&result);
}

#[test]
fn pascal_correctly_spelled_modifiers_remain_in_complete_written_headers() {
    let source = "unit ModifierShapes; interface type TClosed = class sealed end;\n\
        procedure Invoke(Value: Integer); safecall; implementation end.";
    let result = output(source);
    assert_definition(&result, source, "TClosed", EntityKind::Class);
    assert_definition(&result, source, "Invoke", EntityKind::Function);
    assert_definition(&result, source, "Value", EntityKind::Parameter);
    assert_eq!(
        signatures(&result, named(&result, "TClosed")),
        ["TClosed = class sealed "]
    );
    assert_eq!(
        signatures(&result, named(&result, "Invoke")),
        ["procedure Invoke(Value: Integer); safecall;"]
    );
    assert_owner(&result, "Value", "Invoke");
    assert_unqualified(&result);
}

#[test]
fn pascal_preprocessor_alternatives_remain_source_not_a_selected_implementation() {
    let source = "procedure Choose;\n{$ifdef Option}\nbegin end;\n{$else}\nbegin end;\n{$endif}";
    let result = output(source);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Choose")
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "pascal-preprocessor-selection-unavailable")
    );
    assert_unqualified(&result);
}

#[test]
fn pascal_analysis_checks_cancellation_before_native_work() {
    let fixture = Fixture::new(PASCAL, PASCAL.source.as_bytes());
    let budget = limits();
    let (analyzer, parser) = counting_analyzer(PASCAL, ParseTestBehavior::Native);
    let cancelled = deadline();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyzer.prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, PASCAL, &budget),
            &cancelled
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    assert_eq!(parser.invocations.load(Ordering::Relaxed), 0);
}
