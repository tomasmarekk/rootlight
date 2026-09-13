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
fn vba_direct_procedure_calls_bind_written_names_across_invocation_forms() {
    let source = "Sub Visit(ByVal value As Long)\nEnd Sub\nFunction Measure(ByVal value As Long) As Long\nMeasure = value\nEnd Function\nSub Clear()\nEnd Sub\nSub Run()\nCall VISIT(1)\nvisit 2\nDim result As Long\nresult = MEASURE(3)\nClear\nCall CLEAR\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 5, "{:?}", document.occurrences);
    for (site, (marker, target)) in calls.into_iter().zip([
        ("VISIT(1)", "Visit"),
        ("visit 2", "Visit"),
        ("MEASURE(3)", "Measure"),
        ("Clear\n", "Clear"),
        ("CLEAR\n", "Clear"),
    ]) {
        let symbol = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == target)
            .unwrap()
            .id;
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
        let start = source.find(marker).unwrap();
        assert_eq!(
            site.source.span().start_byte(),
            u64::try_from(start).unwrap()
        );
        assert_eq!(
            site.source.span().end_byte(),
            u64::try_from(start + target.len()).unwrap()
        );
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.object == RelationEndpoint::Entity(symbol)
                    && edge.subject == RelationEndpoint::Occurrence(site.id))
        );
    }
}

#[test]
fn vba_recursive_function_invocation_is_not_its_result_variable() {
    let source = "Function Reduce(ByVal value As Long) As Long\nIf value > 0 Then\nReduce = REDUCE(value - 1)\nElse\nReduce = 0\nEnd If\nEnd Function\n";
    let result = output(source);
    let document = result.document();
    let symbol = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Reduce")
        .unwrap()
        .id;
    let calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target, OccurrenceTarget::Resolved { symbol });
    assert_eq!(
        calls[0].source.span().start_byte(),
        u64::try_from(source.find("REDUCE(").unwrap()).unwrap()
    );
    for site in document.occurrences.iter().filter(|site| {
        site.syntax_kind == "vba.identifier.reference"
            && source.get(
                usize::try_from(site.source.span().start_byte()).unwrap()
                    ..usize::try_from(site.source.span().end_byte()).unwrap(),
            ) == Some("Reduce")
    }) {
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    }
}

#[test]
fn vba_conditional_procedure_declarations_do_not_become_unconditional_targets() {
    let source =
        "#If Available Then\nSub Visit()\nEnd Sub\n#End If\nSub Run()\nCall VISIT()\nEnd Sub\n";
    let result = output(source);
    let start = u64::try_from(source.find("VISIT()").unwrap()).unwrap();
    let site = result
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.source.span().start_byte() == start && site.source.span().end_byte() == start + 5
        })
        .unwrap();
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn vba_callable_spelling_does_not_bypass_local_value_shadowing() {
    let source = "Sub Visit(ByVal value As Long)\nEnd Sub\nSub Run(ByVal visit As Object)\nCall VISIT(1)\nEnd Sub\nSub Read()\nDim Visit(2) As Long\nVisit(1) = 2\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::CallSite)
    );
    for marker in ["VISIT(1)", "Visit(1)"] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 5
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("callee value must retain its lexical binding")
        };
        assert!(document.entities.iter().any(|entity| entity.id == symbol
            && matches!(entity.kind, EntityKind::Variable | EntityKind::Parameter)));
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() == start
                    && gap.domain == FactDomain::Relations)
        );
    }
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

fn reference_at<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    source: &str,
    marker: &str,
) -> &'a rootlight_ir::OccurrenceRecord {
    let start = u64::try_from(source.find(marker).expect("reference marker exists")).unwrap();
    document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.identifier.reference"
                && site.source.span().start_byte() == start
        })
        .expect("written reference exists")
}

#[test]
fn vba_parameter_shadows_module_binding_independently_of_written_case() {
    let source = "Private Value As Long\nPublic Sub Update(ByVal value As Long)\nVALUE = value + 1\nEnd Sub\nPublic Sub Other()\nvalue = 2\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "value")
        .unwrap();
    let module_value = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "Value")
        .unwrap();
    for marker in ["VALUE =", "value +"] {
        assert_eq!(
            reference_at(document, source, marker).target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            }
        );
    }
    assert_eq!(
        reference_at(document, source, "value = 2").target,
        OccurrenceTarget::Resolved {
            symbol: module_value.id
        }
    );
}

#[test]
fn vba_procedure_locals_cover_uses_before_their_declaration() {
    let source =
        "Private Count As Long\nPublic Sub Update()\nCOUNT = 1\nDim [count] As Long\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let local = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "count")
        .unwrap();
    assert_eq!(
        reference_at(document, source, "COUNT =").target,
        OccurrenceTarget::Resolved { symbol: local.id }
    );
}

#[test]
fn vba_duplicate_local_names_do_not_expose_an_outer_exact_spelling() {
    let source = "Private Count As Long\nPublic Sub Update()\nDim count As Long\nDim COUNT As Long\nCount = 1\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        reference_at(result.document(), source, "Count =").target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "vba-binding-target-unavailable")
    );
}

#[test]
fn vba_unproven_applications_and_members_keep_scoped_relation_gaps() {
    let source = "Public Sub Update(ByVal source As Object)\nDim result As Long\nresult = source.Value(1)\nEnd Sub\n";
    let result = output(source);
    for detail in [
        "vba-member-target-unavailable",
        "vba-call-or-index-target-unavailable",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == detail),
            "missing {detail}"
        );
    }
}

#[test]
fn vba_conditional_local_declarations_block_outer_fallthrough() {
    let source = "Private Count As Long\nPublic Sub Update()\n#If Wide Then\nDim count As Long\n#End If\nCount = 1\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        reference_at(result.document(), source, "Count =").target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "vba-binding-target-unavailable")
    );
}

#[test]
fn vba_type_label_and_argument_names_do_not_bind_to_local_values() {
    let source = "Public Type Model\nValue As Long\nEnd Type\nPublic Sub Update(ByVal Model As Long, ByVal Finish As Long, ByVal size As Long)\nDim item As Model\nGoTo Finish\nFinish:\nCall Visit(size:=size)\nEnd Sub\n";
    let result = output(source);
    for marker in ["Model\nGoTo", "Finish\nFinish:", "size:="] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert_eq!(site.syntax_kind, "vba.nonvalue_identifier.reference");
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    }
    let parameter = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "size")
        .unwrap();
    assert_eq!(
        reference_at(result.document(), source, "size)").target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
}

#[test]
fn vba_accessor_parameters_remain_separate_environments() {
    let source = "Private stored As Long\nPublic Property Let Value(ByVal item As Long)\nstored = ITEM\nEnd Property\nPublic Property Let Other(ByVal item As Long)\nstored = item\nEnd Property\n";
    let result = output(source);
    let document = result.document();
    let first = reference_at(document, source, "ITEM\n");
    let second = reference_at(document, source, "item\n");
    assert!(matches!(first.target, OccurrenceTarget::Resolved { .. }));
    assert!(matches!(second.target, OccurrenceTarget::Resolved { .. }));
    assert_ne!(first.target, second.target);
}

#[test]
fn vba_parameter_defaults_bind_module_constants_not_body_parameters() {
    let source = "Private Const Fallback As Long = 3\nPublic Sub Update(Optional ByVal Fallback As Long = [FALLBACK])\nFALLBACK = 2\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let constant = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Constant)
        .unwrap();
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter)
        .unwrap();
    let default = document
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.default_constant_identifier.reference")
        .unwrap();
    assert_eq!(
        default.target,
        OccurrenceTarget::Resolved {
            symbol: constant.id
        }
    );
    assert_eq!(
        reference_at(document, source, "FALLBACK = 2").target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
}

#[test]
fn vba_conditional_header_conditions_do_not_bind_runtime_variables() {
    let source = "Private Wide As Boolean\n#If Wide Then\nPublic Sub Update(ByVal value As Long)\n#Else\nPublic Sub Update(ByVal value As Integer)\n#End If\nvalue = 1\nEnd Sub\n";
    let result = output(source);
    let start = u64::try_from(source.find("Wide Then").unwrap()).unwrap();
    let condition = result
        .document()
        .occurrences
        .iter()
        .find(|site| site.source.span().start_byte() == start)
        .unwrap();
    assert_eq!(condition.syntax_kind, "vba.nonvalue_identifier.reference");
    assert!(matches!(
        condition.target,
        OccurrenceTarget::Unresolved { .. }
    ));
}
