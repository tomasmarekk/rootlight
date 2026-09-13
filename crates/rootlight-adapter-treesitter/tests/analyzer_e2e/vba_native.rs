//! VBA authored declarations through the production parser and normalized IR.
//! Source-bound identities are distinct from later project and runtime resolution.

use super::*;

pub(super) const VBA: LanguageCase = LanguageCase {
    name: "vba",
    path: "src/Measure.bas",
    frontend: "tree-sitter-vba-0.14.1",
    source: "Public Function Measure(ByVal value As Long) As Long\nMeasure = value + 1\nEnd Function\n",
    generated: false,
    body_before: "value + 1",
    body_after: "value + 2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(VBA, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, VBA),
        &request(&fixture.snapshot, &fixture.source, VBA, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn vba_authored_declarations_keep_exact_utf8_name_spans() {
    let source = "' λ😀 preserves UTF-8 coordinates\nOption Explicit\nPrivate total As Long, other As String\nPublic Const Limit As Long = 4\nPublic Type Sample\nValue As Long\nEnd Type\nPublic Enum Mode\nIdle = 0\nActive = 1\nEnd Enum\nPublic Event Changed(ByVal current As Long)\nPublic Sub Reset()\nLet total = 0\nEnd Sub\nPublic Function Measure(ByVal value As Long) As Long\nMeasure = value + Limit\nEnd Function\n";
    for source in [source.to_owned(), source.replace('\n', "\r\n")] {
        let result = output(&source);
        for (name, kind) in [
            ("total", EntityKind::Variable),
            ("other", EntityKind::Variable),
            ("Limit", EntityKind::Constant),
            ("Sample", EntityKind::Struct),
            ("Value", EntityKind::Field),
            ("Mode", EntityKind::Enum),
            ("Idle", EntityKind::Constant),
            ("Active", EntityKind::Constant),
            ("Changed", EntityKind::Event),
            ("current", EntityKind::Parameter),
            ("Reset", EntityKind::Function),
            ("Measure", EntityKind::Function),
            ("value", EntityKind::Parameter),
        ] {
            let entities: Vec<_> = result
                .document()
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == name && entity.kind == kind)
                .collect();
            assert_eq!(
                entities.len(),
                1,
                "{name}: {:?}",
                result.document().entities
            );
            let definitions: Vec<_> = result
                .document()
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
            assert_eq!(definitions.len(), 1, "definition for {name}");
            let span = definitions[0].source.span();
            assert_eq!(
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                name
            );
        }
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| !matches!(entity.canonical_name.as_str(), "Let" | "Long" | "String"))
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .all(|site| site.role != OccurrenceRole::CallSite)
        );
    }
}

#[test]
fn vba_property_accessors_have_distinct_source_bound_identities() {
    let source = "Private total As Long\nPublic Property Get Value() As Long\nValue = total\nEnd Property\nPublic Property Let Value(ByVal nextValue As Long)\ntotal = nextValue\nEnd Property\nPublic Property Set Item(ByVal nextItem As Object)\nSet stored = nextItem\nEnd Property\n";
    let result = output(source);
    let properties: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property)
        .collect();
    assert_eq!(properties.len(), 3, "{:?}", result.document().entities);
    assert_eq!(
        properties
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    assert_eq!(
        properties
            .iter()
            .filter(|entity| entity.canonical_name == "Value")
            .count(),
        2
    );
}

#[test]
fn vba_body_only_changes_preserve_declared_symbol_identities() {
    let before = output(VBA.source);
    let after = output(&VBA.source.replace(VBA.body_before, VBA.body_after));
    assert_eq!(symbol_ids(before.document()), symbol_ids(after.document()));
    let fixture = Fixture::new(VBA, VBA.source.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, VBA, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &provider(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let signatures: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature)
        .map(|fact| {
            &VBA.source[usize::try_from(fact.span().start_byte()).unwrap()
                ..usize::try_from(fact.span().end_byte()).unwrap()]
        })
        .collect();
    assert_eq!(
        signatures,
        ["Public Function Measure(ByVal value As Long) As Long"]
    );
}

#[test]
fn vba_quoted_and_type_suffixed_names_preserve_written_occurrences() {
    let source = "Private [Count] As Long\nPrivate total&\nPublic Function [ReadValue]() As Long\n[ReadValue] = [Count] + total&\nEnd Function\n";
    let result = output(source);
    for (name, written) in [
        ("Count", "[Count]"),
        ("total", "total&"),
        ("ReadValue", "[ReadValue]"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap_or_else(|| panic!("missing {name}: {:?}", result.document().entities));
        let occurrence = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .unwrap();
        let span = occurrence.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            written
        );
    }
}

#[test]
fn vba_module_and_parameter_containment_preserve_source_owners() {
    let result = output(VBA.source);
    let document = result.document();
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module)
        .unwrap();
    assert_eq!(module.canonical_name, VBA.path);
    let function = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Measure")
        .unwrap();
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "value")
        .unwrap();
    assert_eq!(
        function.container,
        Some(rootlight_ir::ContainerRef::Entity(module.id))
    );
    assert_eq!(
        parameter.container,
        Some(rootlight_ir::ContainerRef::Entity(function.id))
    );
}

#[test]
fn vba_continued_headers_exclude_bodies_and_preserve_parameter_names() {
    let source = "Public Function Sum( _\nByVal left As Long, _\nByVal right As Long) As Long\nSum = left + right\nEnd Function\n";
    let before = output(source);
    let after = output(&source.replace("left + right", "left - right"));
    assert_eq!(symbol_ids(before.document()), symbol_ids(after.document()));
    for name in ["Sum", "left", "right"] {
        assert!(
            before
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
}

#[test]
fn vba_incomplete_declaration_retains_explicit_parse_gaps() {
    let result = output("Public Function Broken(ByVal value As Long\n");
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(!result.document().skipped_regions.is_empty());
}

#[test]
fn vba_conditional_headers_do_not_silently_choose_one_identity() {
    let source = "#If Wide Then\nPublic Function ReadValue() As Long\n#Else\nPublic Function ReadValue() As Integer\n#End If\nReadValue = 1\nEnd Function\n";
    let fixture = Fixture::new(VBA, source.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, VBA, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &provider(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "vba.identifier.definition")
            .count(),
        2
    );
    let result = output(source);
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(!result.document().skipped_regions.is_empty());
    assert!(
        result
            .document()
            .entities
            .iter()
            .all(|entity| entity.canonical_name != "ReadValue")
    );
}

#[test]
fn vba_external_procedures_retain_names_without_executing_libraries() {
    let source = "Private Declare PtrSafe Function ReadClock Lib \"platform\" Alias \"Clock\" () As LongPtr\nPrivate Declare PtrSafe Sub PauseClock Lib \"platform\" (ByVal ticks As Long)\n";
    let result = output(source);
    for name in ["ReadClock", "PauseClock"] {
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Function && entity.canonical_name == name)
        );
    }
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::CallSite)
    );
}
